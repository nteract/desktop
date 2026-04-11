// Synthetic data generator — produces an Arrow IPC stream file with multiple
// record batches for progressive/chunked loading.
// Run: npx tsx src/generate-data.ts

import { mkdirSync, writeFileSync } from "node:fs";
import { type Table, tableFromArrays, tableToIPC } from "apache-arrow";

const ROW_COUNT = 100_000;
const BATCH_SIZE = 5_000;

// --- Name pools ---

const firstNames = [
  "Mina",
  "Yuki",
  "Somchai",
  "Olga",
  "Ram",
  "Sujin",
  "Émile",
  "Wei",
  "Fatima",
  "Hiroshi",
  "Priya",
  "Dmitri",
  "Aiko",
  "Carlos",
  "Leila",
  "Björn",
  "Amara",
  "Kenji",
  "Nadia",
  "Tariq",
  "Sakura",
  "Ingrid",
  "Rashid",
  "Mei",
  "Alejandra",
  "Kwame",
  "Yuna",
  "Henrik",
  "Zara",
  "Ravi",
  "สมชาย",
  "राम",
  "太郎",
  "수진",
  "محمد",
  "Ólafur",
  "Nguyễn",
];

const lastNames = [
  "Al-Farsi",
  "Tanaka",
  "สมชาย",
  "Petrova",
  "Sharma",
  "Kim",
  "Dubois",
  "Chen",
  "García",
  "Nakamura",
  "Okafor",
  "Johansson",
  "Müller",
  "Singh",
  "Watanabe",
  "López",
  "Andersen",
  "Kowalski",
  "Ali",
  "Sato",
  "Park",
  "Nguyen",
  "Ivanov",
  "Yamamoto",
  "Hernández",
  "Öztürk",
  "Larsson",
  "शर्मा",
  "田中",
  "이",
  "الحسن",
];

// --- Location pools ---

const locations = [
  "القاهرة, Egypt",
  "東京, Japan",
  "กรุงเทพ, Thailand",
  "Москва, Russia",
  "मुम्बई, India",
  "서울, Korea",
  "Paris, France",
  "深圳, China",
  "São Paulo, Brazil",
  "Lagos, Nigeria",
  "Istanbul, Türkiye",
  "Jakarta, Indonesia",
  "Berlin, Germany",
  "Mexico City, Mexico",
  "Nairobi, Kenya",
  "Stockholm, Sweden",
  "Dubai, UAE",
  "Singapore",
  "Toronto, Canada",
  "Sydney, Australia",
  "Lima, Peru",
  "Warsaw, Poland",
  "Hanoi, Vietnam",
  "Accra, Ghana",
  "Reykjavik, Iceland",
  "Buenos Aires, Argentina",
  "Taipei, Taiwan",
  "Riyadh, Saudi Arabia",
  "Zürich, Switzerland",
  "Oslo, Norway",
];

// --- Department pools ---

const departments = [
  "Engineering",
  "Product",
  "Design",
  "Data Science",
  "Infrastructure",
  "Security",
  "DevOps",
  "Research",
  "QA",
  "Platform",
  "Mobile",
  "Frontend",
  "Backend",
  "ML/AI",
  "Analytics",
  "Developer Experience",
  "SRE",
];

// --- Note templates (multilingual, varied lengths) ---

const noteTemplates = [
  // Short
  "Shipped the fix. ✅",
  "On PTO until next week.",
  "LGTM — merging now.",
  "Blocked on API review.",
  "Done ✨",
  "🚀 Deployed to prod.",
  // Medium — English
  "Refactored the authentication middleware to handle edge cases around session token rotation. Tests pass on all three browsers.",
  "The dashboard now renders 2x faster after switching from DOM measurement to pretext for height calculation.",
  "Investigating a regression in the search indexer — looks like the Urdu tokenizer is splitting compound words incorrectly.",
  "Wrote a comprehensive test suite for the new billing module. Coverage went from 43% to 91%.",
  "Pair-programmed with the new hire on the GraphQL migration. They picked it up fast.",
  // Medium — mixed script
  "CJK行の折り返しが修正されました。句読点の結合ルールが正しく動作しています。",
  "แก้ไขการตัดคำภาษาไทยในระบบค้นหาแล้ว ผลลัพธ์ดีขึ้นมาก",
  "تم تحسين أداء واجهة المستخدم العربية. النص ثنائي الاتجاه يعمل بشكل صحيح الآن.",
  "देवनागरी स्क्रिप्ट में संयुक्ताक्षर विभाजन अब सही तरीके से काम कर रहा है।",
  'Mixed text wrapping: "hello" 世界 مرحبا สวัสดี works across all 4 scripts now.',
  "URL wrapping for https://example.com/reports/q3?lang=ar&mode=full splits correctly at the query delimiter.",
  "Emoji ZWJ sequences like 👨‍👩‍👧‍👦 and 🏳️‍🌈 now measure correctly on all browsers.",
  // Long
  "Spent the day profiling the virtual scroll implementation. The bottleneck was not rendering — it was layout reflow from getBoundingClientRect calls. Switched to pretext for height prediction and the 99th percentile frame time dropped from 34ms to 4ms. Next step: verify this holds with the Arabic/Thai mixed-text corpus where grapheme boundaries get tricky.",
  "The new preprocessing pipeline handles Arabic punctuation-plus-mark clusters, CJK kinsoku prohibitions, and Southeast Asian word boundaries all in one pass. Previously these were three separate post-processing steps that sometimes conflicted. The unified approach also opened up a clean path for soft-hyphen support.",
  'Completed the accessibility audit for the data grid component. Screen readers now announce column headers, sort state, and row count. The virtual scroll needed a live region to announce "showing rows 1-50 of 10,000" on scroll. Also fixed a contrast ratio issue in the header resize handles.',
  "Debated whether to use Arrow IPC or Parquet for the client-side data layer. Arrow IPC won because it is zero-copy — the browser can memory-map the ArrayBuffer directly into typed arrays without parsing. For a 50k-row table with 6 string columns, load time went from 340ms (JSON) to 12ms (Arrow IPC).",
  "ソフトハイフンの処理を修正しました。非表示時はゼロ幅で、改行選択時にはハイフンが正しく表示されます。layoutWithLines()の出力でline.textに末尾のハイフンが含まれるようになりました。全ブラウザでテスト済み。",
  "تم مراجعة الشفرة البرمجية لوحدة التخطيط. أصبح النص العربي مع علامات الترقيم يلتف بشكل صحيح. تم اختبار ذلك مع نصوص متعددة اللغات تجمع بين العربية والإنجليزية واليابانية.",
];

const statuses = [
  "Active",
  "On Leave",
  "Contractor",
  "Probation",
  "Remote",
  "Hybrid",
];
const priorities = ["P0", "P1", "P2", "P3", "P4"];

const emailDomains = [
  "company.io",
  "acme.com",
  "org.dev",
  "example.co",
  "corp.net",
  "team.work",
  "devs.io",
  "eng.co",
  "labs.dev",
  "hq.org",
];

// --- Deterministic PRNG (mulberry32) ---

function mulberry32(seed: number) {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const rand = mulberry32(42);

function pick<T>(arr: T[]): T {
  return arr[Math.floor(rand() * arr.length)];
}

// --- Generate batches ---

const NOW = Date.now();
const FIVE_YEARS_MS = 5 * 365.25 * 86_400_000;

function sanitizeEmail(s: string): string {
  return s
    .normalize("NFD")
    .replace(/[\u0300-\u036f]/g, "") // strip diacritics
    .replace(/[^a-zA-Z0-9]/g, "") // ASCII only
    .toLowerCase();
}

function generateBatch(startId: number, count: number) {
  const ids = new Int32Array(count);
  const names: string[] = [];
  const locationCol: string[] = [];
  const departmentCol: string[] = [];
  const notes: string[] = [];
  const statusCol: string[] = [];
  const priorityCol: string[] = [];
  const scores = new Float64Array(count);
  const emails: string[] = [];
  const verified: boolean[] = [];
  const joined = new Float64Array(count);
  const chaos: (number | null)[] = [];

  for (let i = 0; i < count; i++) {
    ids[i] = startId + i + 1;
    const first = pick(firstNames);
    const last = pick(lastNames);
    names.push(`${first} ${last}`);
    locationCol.push(pick(locations));
    departmentCol.push(pick(departments));
    notes.push(pick(noteTemplates));
    statusCol.push(pick(statuses));
    priorityCol.push(pick(priorities));
    scores[i] = Math.round(rand() * 10000) / 100;
    emails.push(
      `${sanitizeEmail(first)}.${sanitizeEmail(last)}@${pick(emailDomains)}`,
    );
    verified.push(rand() < 0.72);
    joined[i] = Math.round(NOW - rand() * FIVE_YEARS_MS);

    // The chaos column: mostly normal values, sprinkled with horrors
    const roll = rand();
    if (roll < 0.03) chaos.push(null);
    else if (roll < 0.05) chaos.push(NaN);
    else if (roll < 0.065) chaos.push(Infinity);
    else if (roll < 0.08) chaos.push(-Infinity);
    else if (roll < 0.09) chaos.push(0);
    else if (roll < 0.095) chaos.push(-0);
    else if (roll < 0.1) chaos.push(Number.MAX_SAFE_INTEGER);
    else if (roll < 0.105) chaos.push(Number.MIN_SAFE_INTEGER);
    else if (roll < 0.11) chaos.push(Number.EPSILON);
    else if (roll < 0.115)
      chaos.push(4.9e-324); // Number.MIN_VALUE
    else chaos.push(Math.round((rand() * 200 - 100) * 100) / 100);
  }

  return tableFromArrays({
    id: ids,
    name: names,
    location: locationCol,
    department: departmentCol,
    note: notes,
    status: statusCol,
    priority: priorityCol,
    score: scores,
    email: emails,
    verified,
    joined,
    chaos,
  });
}

// --- Build multi-batch table and write IPC stream ---
// Generate first batch to establish schema, then concat the rest

const allBatchTables: Table[] = [];
for (let offset = 0; offset < ROW_COUNT; offset += BATCH_SIZE) {
  const count = Math.min(BATCH_SIZE, ROW_COUNT - offset);
  allBatchTables.push(generateBatch(offset, count));
}

// Table.concat preserves batches and reconciles schemas
const table = allBatchTables.reduce((acc, t) => acc.concat(t));

mkdirSync("public", { recursive: true });
const ipc = tableToIPC(table, "stream");
writeFileSync("public/data.arrow", ipc);

console.log(
  `Generated ${ROW_COUNT} rows in ${table.batches.length} batches → public/data.arrow ` +
    `(${(ipc.byteLength / 1024 / 1024).toFixed(1)} MB)`,
);
