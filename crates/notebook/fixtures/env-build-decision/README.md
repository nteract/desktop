# Environment Build Decision Fixtures

These notebooks exercise the `environment.yml` missing-env UX.

Open the notebooks with Cmd-O from a dev build of the notebook app.

Expected behavior:

- `01-missing-named-env/awaiting-env-build.ipynb`: shows the environment build decision dialog for `nteract-qa-missing-env-alpha`.
- `02-missing-named-env-yaml/awaiting-env-build-yaml.ipynb`: shows the same decision flow, using `environment.yaml`.
- `03-missing-named-env-with-pip/awaiting-env-build-pip.ipynb`: shows the same decision flow with a pip subsection.
- `04-no-name-control/no-name-control.ipynb`: control case. The env file has no `name` or `prefix`, so it must not show the named missing-env dialog.

Reset any created test envs before rerunning the missing-env cases:

```bash
conda env remove -n nteract-qa-missing-env-alpha -y || true
conda env remove -n nteract-qa-missing-env-beta -y || true
conda env remove -n nteract-qa-missing-env-pip -y || true
```
