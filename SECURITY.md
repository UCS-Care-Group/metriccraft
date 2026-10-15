# Security policy

MetricCraft opens files and runs expressions from sources you may not control (CSV, XLSX, Parquet,
Projects, Packages, DAX and M). A crash, hang, runaway memory use or file access outside what was
asked for, caused by such input, is a security bug.

## Reporting

Please report privately through GitHub's
[private vulnerability reporting](https://github.com/UCS-Care-Group/metriccraft/security/advisories/new),
not in a public issue. Include the input file or expression (synthetic, please: no real data) and
the version or commit.

We aim to acknowledge reports within a week. MetricCraft is pre-alpha, so only the latest `main`
is supported.
