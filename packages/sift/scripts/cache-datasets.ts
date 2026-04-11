/**
 * Download HuggingFace datasets into public/datasets/ for local-first serving.
 *
 * Usage: npx tsx scripts/cache-datasets.ts [dataset-id...]
 *
 * With no args, downloads all HF datasets. Pass IDs to download specific ones:
 *   npx tsx scripts/cache-datasets.ts spotify titanic
 */

import { writeFileSync, mkdirSync, existsSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const DATASETS_DIR = resolve(__dirname, '../public/datasets')

interface DatasetEntry {
  id: string
  label: string
  source: string
  path: string
  config?: string
  split?: string
}

// Inline the HF datasets to avoid import issues with the TS source
const HF_DATASETS: DatasetEntry[] = [
  { id: 'spotify', label: 'Spotify Tracks', source: 'huggingface', path: 'maharshipandya/spotify-tracks-dataset' },
  { id: 'airbnb-nyc', label: 'NYC Airbnb', source: 'huggingface', path: 'gradio/NYC-Airbnb-Open-Data' },
  { id: 'adult-census', label: 'Adult Census Income', source: 'huggingface', path: 'scikit-learn/adult-census-income' },
  { id: 'credit-card', label: 'Credit Card', source: 'huggingface', path: 'imodels/credit-card' },
  { id: 'gsm8k', label: 'GSM8K', source: 'huggingface', path: 'openai/gsm8k', config: 'main' },
  { id: 'heart-failure', label: 'Heart Failure', source: 'huggingface', path: 'mstz/heart_failure' },
  { id: 'titanic', label: 'Titanic', source: 'huggingface', path: 'phihung/titanic' },
]

async function resolveParquetUrl(dataset: string, config = 'default', split = 'train'): Promise<string> {
  const apiUrl = `https://datasets-server.huggingface.co/parquet?dataset=${encodeURIComponent(dataset)}`
  const resp = await fetch(apiUrl)
  if (!resp.ok) throw new Error(`HuggingFace API error: ${resp.status} ${resp.statusText}`)
  const data = await resp.json()

  const files = data.parquet_files as Array<{ config: string; split: string; url: string }>
  if (!files?.length) throw new Error(`No Parquet files found for ${dataset}`)

  let match = files.find(f => f.config === config && f.split === split)
  if (!match) match = files.find(f => f.split === split)
  if (!match) match = files[0]
  return match.url
}

async function downloadDataset(entry: DatasetEntry) {
  const outPath = resolve(DATASETS_DIR, `${entry.id}.parquet`)

  if (existsSync(outPath)) {
    console.log(`  skip ${entry.id} (already cached)`)
    return
  }

  console.log(`  downloading ${entry.id} (${entry.path})...`)
  const url = await resolveParquetUrl(entry.path, entry.config)
  const resp = await fetch(url)
  if (!resp.ok) throw new Error(`Failed to fetch: ${resp.status}`)
  const bytes = new Uint8Array(await resp.arrayBuffer())
  writeFileSync(outPath, bytes)
  const mb = (bytes.length / 1024 / 1024).toFixed(1)
  console.log(`  saved ${entry.id}.parquet (${mb} MB)`)
}

async function main() {
  mkdirSync(DATASETS_DIR, { recursive: true })

  const args = process.argv.slice(2)
  const targets = args.length > 0
    ? HF_DATASETS.filter(d => args.includes(d.id))
    : HF_DATASETS

  if (targets.length === 0) {
    console.error(`No matching datasets. Available: ${HF_DATASETS.map(d => d.id).join(', ')}`)
    process.exit(1)
  }

  console.log(`Caching ${targets.length} dataset(s) into public/datasets/\n`)

  for (const entry of targets) {
    try {
      await downloadDataset(entry)
    } catch (err) {
      console.error(`  FAILED ${entry.id}: ${err instanceof Error ? err.message : err}`)
    }
  }

  console.log('\nDone. Start the dev server to use cached datasets.')
}

main()
