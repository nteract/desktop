/** Catalog of datasets available in the picker. */

export interface DatasetEntry {
  id: string;
  label: string;
  description: string;
  /** 'local' = fetch from BASE_URL, 'huggingface' = fetch Parquet from HF */
  source: "local" | "huggingface";
  /** For local: relative path under BASE_URL. For HF: "{owner}/{name}" */
  path: string;
  /** HF config name (default: "default") */
  config?: string;
  /** HF split (default: "train") */
  split?: string;
  /** Approximate row count for display */
  rows?: string;
  /** Column type overrides keyed by column name */
  typeOverrides?: Record<string, "numeric" | "categorical" | "timestamp" | "boolean">;
}

export const DATASETS: DatasetEntry[] = [
  {
    id: "generated",
    label: "Generated (100k chaos)",
    description:
      "100k synthetic rows with multilingual text, nulls, NaN, ±Infinity, edge-case floats",
    source: "local",
    path: "data.arrow",
    rows: "100,000",
    typeOverrides: { joined: "timestamp" },
  },
  {
    id: "spotify",
    label: "Spotify Tracks",
    description:
      "114k tracks — booleans, floats (danceability, energy…), unicode artists, 21 columns",
    source: "huggingface",
    path: "maharshipandya/spotify-tracks-dataset",
    rows: "~114,000",
  },
  {
    id: "airbnb-nyc",
    label: "NYC Airbnb",
    description:
      "49k listings — nulls in dates & reviews, lat/lon floats, categorical neighborhoods",
    source: "huggingface",
    path: "gradio/NYC-Airbnb-Open-Data",
    rows: "~49,000",
  },
  {
    id: "adult-census",
    label: "Adult Census Income",
    description:
      "49k rows — many categorical columns (workclass, education, occupation…), good for category bars",
    source: "huggingface",
    path: "scikit-learn/adult-census-income",
    rows: "~49,000",
  },
  {
    id: "credit-card",
    label: "Credit Card (34 cols)",
    description: "30k rows × 34 columns — tests wide tables, all numeric/float",
    source: "huggingface",
    path: "imodels/credit-card",
    rows: "~30,000",
  },
  {
    id: "gsm8k",
    label: "GSM8K (long text)",
    description: "8.8k math word problems with multi-line solutions — stress-tests row height calc",
    source: "huggingface",
    path: "openai/gsm8k",
    config: "main",
    rows: "~8,800",
  },
  {
    id: "heart-failure",
    label: "Heart Failure (tiny)",
    description: "299 rows, 13 cols — 5 boolean columns + mixed numeric, quick load",
    source: "huggingface",
    path: "mstz/heart_failure",
    rows: "299",
  },
  {
    id: "titanic",
    label: "Titanic (mixed + nulls)",
    description:
      "891 passengers — mixed types, lots of nulls in age/cabin, classic edge-case dataset",
    source: "huggingface",
    path: "phihung/titanic",
    rows: "891",
  },
];
