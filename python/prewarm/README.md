# prewarm

Warm up Python environments by importing packages and triggering their
one-time side effects — font caches, C extension loading, BLAS discovery,
and more.

## Usage

```bash
# Warm up via IPython (warms IPython's own init + packages)
prewarm matplotlib pandas numpy

# Or as a module
python -m prewarm matplotlib pandas numpy

# Skip IPython, just import directly
prewarm --no-ipython matplotlib pandas
```

## API

```python
from prewarm import warm

# Boot IPython, import modules, exit
warm(["matplotlib", "pandas"])

# Just import, no IPython
warm(["matplotlib", "pandas"], ipython=False)
```

## Why?

First imports of heavy packages like matplotlib and pandas are slow — they
build font caches, load C extensions, discover BLAS libraries, etc.
Running `prewarm` in a fresh environment triggers all of these one-time
costs up front so subsequent imports are fast.

When run with IPython (the default), it also warms IPython's startup path:
traitlets configuration, magic commands, tab completion, and the display
system. This is especially useful for Jupyter kernels where IPython boot
time is part of the perceived startup latency.
