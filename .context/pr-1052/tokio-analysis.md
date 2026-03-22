# How the Daemon Uses Tokio — and Where It Could Get Simpler

## The Big Picture

So here's how the daemon is structured. Each notebook gets a "room" — a
NotebookRoom. The room holds the Automerge document, the runtime state
document, the kernel, and a bunch of broadcast channels. When clients
connect, each one gets a loop — a big select loop with six arms — that
multiplexes between incoming client messages, document changes, state
changes, presence updates, kernel output, and a periodic cleanup timer.

When a kernel is running, there are four or five long-lived tasks per
kernel. An IOPub listener that reads Jupyter messages and routes them to
the right places. A shell reader that handles execution replies, history,
and completions. A process watcher that detects when the kernel dies. A
heartbeat monitor that pings the kernel every five seconds. And a command
processor that handles execution lifecycle events like "this cell is done"
or "the kernel died."

On top of that, each room has a persist debouncer, an autosave debouncer,
and a file watcher. So at any given time, a single notebook might have ten
or more concurrent tasks all touching shared state.

## The Lock Landscape

Here's where it gets interesting. The codebase uses two kinds of mutexes
deliberately. Standard library mutexes for things that need microsecond
lookups — the cell ID map, pending history requests, pending completions.
These are never held across an await point. Then Tokio mutexes and
read-write locks for everything else — the document, the state document,
the kernel, stream terminals, presence state.

The kernel itself is behind an Arc Tokio Mutex Option RoomKernel. Every
request that touches the kernel — queue a cell, interrupt, shutdown,
history, completions — has to acquire that mutex. And the command
processor task also acquires it for every execution lifecycle event.

The Automerge document is behind an Arc Tokio RwLock, which is nice
because multiple peers can read simultaneously, but writes are serialized.

## Where the Complexity Hurts

The IOPub listener task is the monster. It's about 700 lines inside a
single loop match block. For every Jupyter message it receives, it might
lock the cell ID map to figure out which cell the message belongs to.
Then lock the stream terminals to feed text through the terminal emulator.
Then acquire a write lock on the document to upsert the output. And
inside that document write lock, it sometimes re-locks the stream
terminals to update output state. That's a nested lock — document write
lock held while waiting for the terminals lock. It works today because
only the IOPub task does this nesting, but it's fragile. If anyone else
ever holds terminals and then tries to write to the document, that's a
deadlock.

The kernel lock contention is another pain point. When the auto-launch
function starts a kernel, it holds the kernel mutex for the entire launch
sequence — environment setup, process spawn, the kernel info handshake.
That can take hundreds of milliseconds. During that time, any cell
execution request just blocks.

There's also a subtle race in the cell ID map. The IOPub task reads it
to route messages. The shell reader reads it to route execution replies.
And process next writes it to register new mappings. The cleanup strategy
is intentionally deferred — you don't clean up the mapping when an
execution finishes, because the shell and IOPub channels race and both
need the mapping. Instead, cleanup happens when a cell is re-executed.
It's correct but you really have to read the comments to understand why.

## What the New Architecture Could Simplify

Here's what I think gets better with the execution log design we talked
about.

First, the command processor task could get much simpler. Right now it
handles execution done, cell error, and kernel died — and for each one,
it acquires the kernel lock, mutates the queue, updates the state
document, and broadcasts changes. With the new design, the daemon just
writes to the executions map in the runtime state document. Status
changes are CRDT writes, not channel messages processed by a separate
task. You might not even need the command processor as a separate task.

Second, the cell ID map could become simpler. Right now it maps Jupyter
message IDs to cell ID and execution ID pairs, and the cleanup strategy
is tricky because of the IOPub and shell race. With execution IDs as a
first-class concept stored in the CRDT, the daemon could just look up
the execution by ID in the state document rather than maintaining a
side-channel map. The map might still be useful for performance — CRDT
lookups are slower than a HashMap — but it becomes a cache rather than
the source of truth.

Third, the queue management gets cleaner. Right now RoomKernel owns a
VecDeque of queued cells and an executing tuple, and set_queue writes
parallel lists into the CRDT. With a map of maps in the runtime state
document, the queue is just the set of executions with status "queued",
ordered by the daemon's internal list. The CRDT reflects reality rather
than being a parallel bookkeeping system.

Fourth, and this is the big one — the IOPub task's output routing could
be restructured. Right now it writes outputs directly into the notebook
document cells, interleaving CRDT mutations with terminal emulation and
blob storage. With outputs keyed by execution ID in the runtime state
document, the write path is simpler — just append to the outputs list
for this execution ID. No need to find the right cell, figure out the
output index, or deal with stream upserts in the same lock scope as
terminal state.

## What Probably Stays Complex

The select loop for each peer isn't going away. You still need to
multiplex client messages with state changes. The broadcast channels are
still the right pattern for fan-out.

The IOPub task is still going to be the hottest path in the system. Every
kernel output flows through it. But the work it does per message could
get simpler if outputs go into a flat list rather than being upserted
into nested cell structures.

The lock nesting in the IOPub task — that's the thing I'd focus on
eliminating first. Even if you don't change the architecture, pulling
the stream terminals lock out of the document write lock scope would
reduce the deadlock surface area significantly. Do the terminal work
first, collect the result, then take the document lock and write it.

## A Note on the Watch Channel

One thing I thought was really clever — the persist channel is a Tokio
watch channel, not an mpsc. Watch channels have "latest value wins"
semantics. When the IOPub task is processing a burst of outputs, each
one sends on the persist channel, but the persist debouncer only sees
the latest signal. This means rapid outputs don't create an unbounded
queue of persist requests. It's a small thing but it shows careful
thinking about back-pressure.

## Summary

The Tokio usage isn't bad — it's actually well-reasoned. The lock
ordering is mostly consistent, the channel choices are appropriate, and
the separation between the IOPub task, shell reader, and command
processor makes sense. The complexity is inherent in what the system
does — it's a multi-client, multi-document, real-time collaboration
server that also manages a Jupyter kernel.

But the new execution log architecture could meaningfully reduce the
moving parts. Fewer channels, simpler queue management, flatter output
writes, and the potential to collapse the command processor into the
IOPub task's existing flow. The CRDT becomes the coordination mechanism
instead of channels and shared mutable state, which is kind of the
whole point of using CRDTs in the first place.