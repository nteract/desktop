//! Typosquatting detection for package names.
//!
//! Warns users when a package name is suspiciously similar to a popular package,
//! helping prevent supply chain attacks via typosquatting (e.g., `numppy` instead of `numpy`).

use serde::{Deserialize, Serialize};
use strsim::levenshtein;

/// A warning about a potentially typosquatted package name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TyposquatWarning {
    /// The package name that was checked.
    pub package: String,
    /// The popular package it might be confused with.
    pub similar_to: String,
    /// The edit distance between the names.
    pub distance: usize,
}

/// Top PyPI packages by download count.
/// This list is used to detect typosquatting attempts.
///
/// Source: https://hugovk.github.io/top-pypi-packages/ (regularly updated)
const POPULAR_PACKAGES: &[&str] = &[
    // Top 100 most downloaded packages (roughly)
    "boto3",
    "botocore",
    "urllib3",
    "requests",
    "setuptools",
    "charset-normalizer",
    "certifi",
    "idna",
    "typing-extensions",
    "python-dateutil",
    "s3transfer",
    "packaging",
    "aiobotocore",
    "pyyaml",
    "numpy",
    "six",
    "cryptography",
    "pip",
    "jmespath",
    "s3fs",
    "fsspec",
    "cffi",
    "attrs",
    "pycparser",
    "wheel",
    "zipp",
    "importlib-metadata",
    "aiohttp",
    "pyasn1",
    "multidict",
    "pandas",
    "platformdirs",
    "yarl",
    "rsa",
    "pytz",
    "google-api-core",
    "async-timeout",
    "awscli",
    "protobuf",
    "googleapis-common-protos",
    "filelock",
    "wrapt",
    "markupsafe",
    "frozenlist",
    "colorama",
    "aiosignal",
    "click",
    "jinja2",
    "jsonschema",
    "tomli",
    "pyparsing",
    "pydantic",
    "grpcio",
    "pyarrow",
    "sqlalchemy",
    "tqdm",
    "docutils",
    "google-auth",
    "werkzeug",
    "pillow",
    "scipy",
    "decorator",
    "pluggy",
    "greenlet",
    "cachetools",
    "exceptiongroup",
    "tzdata",
    "pytest",
    "iniconfig",
    "flask",
    "pyjwt",
    "google-cloud-storage",
    "lxml",
    "pyopenssl",
    "psutil",
    "oauthlib",
    "soupsieve",
    "beautifulsoup4",
    "google-cloud-core",
    "requests-oauthlib",
    "httplib2",
    "pygments",
    "isodate",
    "openpyxl",
    "networkx",
    "et-xmlfile",
    "httpx",
    "sniffio",
    "anyio",
    "httpcore",
    "h11",
    "distlib",
    "virtualenv",
    "matplotlib",
    "scikit-learn",
    "joblib",
    "threadpoolctl",
    "pynacl",
    "bcrypt",
    "paramiko",
    // Additional commonly targeted packages
    "tensorflow",
    "torch",
    "keras",
    "opencv-python",
    "opencv-contrib-python",
    "selenium",
    "scrapy",
    "django",
    "fastapi",
    "uvicorn",
    "gunicorn",
    "celery",
    "redis",
    "pymongo",
    "psycopg2",
    "mysqlclient",
    "elasticsearch",
    "boto",
    "aws-cdk-lib",
    "black",
    "flake8",
    "mypy",
    "isort",
    "pylint",
    "coverage",
    "nose",
    "mock",
    "faker",
    "factory-boy",
    "hypothesis",
    "httpretty",
    "responses",
    "moto",
    "ipython",
    "jupyter",
    "notebook",
    "jupyterlab",
    "ipykernel",
    "ipywidgets",
    "anywidget",
    "nbformat",
    "nbconvert",
    "traitlets",
    "rich",
    "typer",
    "pydantic-settings",
    "python-dotenv",
    "python-multipart",
    "starlette",
    "aiofiles",
    "orjson",
    "ujson",
    "msgpack",
    "cloudpickle",
    "dill",
    "transformers",
    "tokenizers",
    "huggingface-hub",
    "accelerate",
    "safetensors",
    "datasets",
    "evaluate",
    "timm",
    "torchvision",
    "torchaudio",
    "lightning",
    "wandb",
    "mlflow",
    "ray",
    "dask",
    "xarray",
    "zarr",
    "numba",
    "cython",
    // Scientific computing & math
    "sympy",
    "mpmath",
    "astropy",
    "biopython",
    "statsmodels",
    "h5py",
    "netcdf4",
    "scikit-image",
    // Data visualization
    "seaborn",
    "plotly",
    "bokeh",
    "graphviz",
    // Data engineering
    "polars",
    "duckdb",
    "sqlparse",
    "alembic",
    "peewee",
    "dataset",
    // NLP & text processing
    "spacy",
    "nltk",
    "gensim",
    "regex",
    // Build tools & linters
    "poetry",
    "ruff",
    "pre-commit",
    "tox",
    "nox",
    "bandit",
    "hatch",
    "flit",
    "maturin",
    "pdm",
    "pycodestyle",
    "autopep8",
    "pyflakes",
    "yapf",
    "rope",
    // Web frameworks & networking
    "twisted",
    "tornado",
    "sanic",
    "falcon",
    "bottle",
    "websockets",
    "uvloop",
    "httptools",
    "grpcio-tools",
    // Async
    "trio",
    // Cloud & orchestration
    "ansible",
    "fabric",
    "invoke",
    "prefect",
    "dagster",
    "luigi",
    "airflow",
    // Serialization & validation
    "marshmallow",
    "pydantic-core",
    "cattrs",
    "toml",
    "tomli-w",
    // Utilities
    "arrow",
    "pendulum",
    "chardet",
    "tenacity",
    "loguru",
    "structlog",
    "watchdog",
    "tabulate",
    "fire",
    "more-itertools",
    "toolz",
    "boltons",
    "colorlog",
    "jedi",
    "pika",
    // Type checking & IDE
    "pyright",
    "beartype",
    // GUI frameworks
    "pyqt5",
    "pyside6",
    "kivy",
    "pygame",
    // Image & media
    "imageio",
    "librosa",
    // Documentation
    "sphinx",
    "mkdocs",
    // Messaging
    "kombu",
    "dramatiq",
    // Network analysis
    "scapy",
];

use notebook_doc::metadata::extract_package_name;

/// Normalize a package name for comparison.
/// PyPI considers `_`, `-`, and `.` as equivalent, and is case-insensitive.
fn normalize_name(name: &str) -> String {
    name.to_lowercase().replace(['_', '.'], "-")
}

/// Check if a package name is suspiciously similar to a popular package.
///
/// Returns `Some(TyposquatWarning)` if the package name is within edit distance
/// threshold of a popular package (but not an exact match).
pub fn check_typosquat(package: &str) -> Option<TyposquatWarning> {
    let pkg_name = extract_package_name(package);
    let normalized = normalize_name(&pkg_name);

    // Skip if the package is itself a popular package (exact match)
    for &popular in POPULAR_PACKAGES {
        if normalize_name(popular) == normalized {
            return None;
        }
    }

    // Check edit distance against popular packages
    let threshold = match normalized.len() {
        0..=3 => 1, // Very short names: only 1 edit allowed
        4..=6 => 2, // Short names: 2 edits
        _ => 3,     // Longer names: 3 edits
    };

    let mut best_match: Option<(&str, usize)> = None;

    for &popular in POPULAR_PACKAGES {
        let popular_normalized = normalize_name(popular);
        let distance = levenshtein(&normalized, &popular_normalized);

        // Skip exact matches (handled above)
        if distance == 0 {
            continue;
        }

        // Check if within threshold and better than current best
        if distance <= threshold && best_match.as_ref().is_none_or(|(_, d)| distance < *d) {
            best_match = Some((popular, distance));
        }
    }

    best_match.map(|(similar_to, distance)| TyposquatWarning {
        package: pkg_name.to_string(),
        similar_to: similar_to.to_string(),
        distance,
    })
}

/// Check multiple packages for typosquatting.
///
/// Returns warnings for any packages that look like typosquats.
pub fn check_packages(packages: &[String]) -> Vec<TyposquatWarning> {
    packages
        .iter()
        .filter_map(|pkg| check_typosquat(pkg))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match_no_warning() {
        assert!(check_typosquat("numpy").is_none());
        assert!(check_typosquat("pandas").is_none());
        assert!(check_typosquat("requests").is_none());
    }

    #[test]
    fn test_with_version_specifier() {
        assert!(check_typosquat("numpy>=1.20").is_none());
        assert!(check_typosquat("pandas[sql]>=2.0").is_none());
    }

    #[test]
    fn test_typosquat_detection() {
        // Common typosquats
        let warning = check_typosquat("numppy").expect("should detect typosquat");
        assert_eq!(warning.similar_to, "numpy");

        let warning = check_typosquat("padas").expect("should detect typosquat");
        assert_eq!(warning.similar_to, "pandas");

        let warning = check_typosquat("requets").expect("should detect typosquat");
        assert_eq!(warning.similar_to, "requests");
    }

    #[test]
    fn test_normalization() {
        // PyPI normalizes these characters
        assert!(check_typosquat("Numpy").is_none()); // Case insensitive
        assert!(check_typosquat("typing_extensions").is_none()); // _ == -
    }

    #[test]
    fn test_anywidget_not_flagged() {
        // anywidget is a real package, not a typosquat of ipywidgets
        assert!(check_typosquat("anywidget").is_none());
    }

    #[test]
    fn test_known_false_positive_pairs() {
        // These are all legitimate packages that are close in edit distance
        // to other popular packages — they must NOT be flagged.
        assert!(check_typosquat("sympy").is_none()); // vs numpy/scipy/mypy
        assert!(check_typosquat("scapy").is_none()); // vs scipy/scrapy
        assert!(check_typosquat("spacy").is_none()); // vs scipy
        assert!(check_typosquat("cattrs").is_none()); // vs attrs
        assert!(check_typosquat("toml").is_none()); // vs tomli
        assert!(check_typosquat("h5py").is_none()); // vs mypy
        assert!(check_typosquat("dataset").is_none()); // vs datasets
        assert!(check_typosquat("arrow").is_none()); // vs pyarrow
        assert!(check_typosquat("airflow").is_none()); // vs pillow/mlflow
        assert!(check_typosquat("biopython").is_none()); // vs ipython
        assert!(check_typosquat("astropy").is_none()); // vs scrapy
        assert!(check_typosquat("pyright").is_none()); // vs pylint
        assert!(check_typosquat("pygame").is_none()); // vs pyyaml
        assert!(check_typosquat("pika").is_none()); // vs pip
        assert!(check_typosquat("jedi").is_none()); // vs redis
        assert!(check_typosquat("rope").is_none()); // vs nose
        assert!(check_typosquat("boltons").is_none()); // vs boto3/boto
    }

    #[test]
    fn test_typosquats_of_new_entries() {
        // Typosquats of newly added packages should still be detected
        let warning = check_typosquat("symppy").expect("should detect typosquat of sympy");
        assert_eq!(warning.similar_to, "sympy");

        let warning = check_typosquat("polrs").expect("should detect typosquat of polars");
        assert_eq!(warning.similar_to, "polars");
    }

    #[test]
    fn test_unrelated_package_no_warning() {
        // Random package names shouldn't trigger warnings
        assert!(check_typosquat("my-custom-package").is_none());
        assert!(check_typosquat("foobarqux").is_none());
    }

    #[test]
    fn test_extract_package_name() {
        assert_eq!(extract_package_name("pandas>=2.0"), "pandas");
        assert_eq!(extract_package_name("numpy[extra]"), "numpy");
        assert_eq!(extract_package_name("requests~=2.28"), "requests");
        assert_eq!(extract_package_name("torch @ https://..."), "torch");
    }
}
