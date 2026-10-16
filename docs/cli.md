# Command line: `metriccraft-cli`

Last updated: 2026-10-16.

`metriccraft-cli` opens a Project folder (or its `project.mcraft.json`) or a `.mcraft` Package
without a window and runs [commands](commands.md) on it. It has no GUI libraries, so it runs on
servers and in CI. It writes nothing unless you pass `--save` (back where it was opened) or
`--save-as PATH` (a folder, or a Package when `PATH` ends in `.mcraft`).

| Verb                                          | Does                                                                |
|-----------------------------------------------|---------------------------------------------------------------------|
| `describe P`                                  | Tables, Columns, Measures, Relationships, Queries and Pages as JSON |
| `refresh P [--save]`                          | Reloads every Query from its Sources; rows per Table on stderr      |
| `dax P QUERY [--format F] [--max-rows N]`     | Runs a DAX query (one `EVALUATE`) with the Model's Measures         |
| `render P --out X.png [--page P] [--scale S]` | Draws a Page as PNG (`--width PX` instead of `--scale`)             |
| `run P ID [--params JSON] [--save]`           | Runs one command; prints its result, if it has one, as JSON         |
| `script P FILE [--save]`                      | Runs a JSON-lines file of `{"run": id, "params": {…}}`; `-` = stdin |
| `commands [--json]`                           | Every command with its params                                       |

`--save-as PATH` works wherever `--save` does.

- **DAX output:** a table on a terminal, CSV when piped, or `--format table|csv|json`. BLANK is
  empty (`null` in JSON); `--max-rows` defaults to 10,000.
- **Changes** are recorded as made by the CLI. `file.*` commands are refused in `run` and
  `script`: saving is what `--save` is for.
- **A script** stops at the first line that fails and saves nothing.
- **Progress:** rows read per Query show on stderr while a load runs, on a terminal.
- **Ctrl+C** cancels the running load or query, which changes nothing; a second Ctrl+C quits.
- **Exit codes:** 0 success, 1 a command failed, 2 a usage error.

```bash
metriccraft-cli dax Sales/ 'EVALUATE SUMMARIZECOLUMNS(Sales[Region], "Total", [Total])'
metriccraft-cli refresh Sales/ --save
metriccraft-cli render Sales.mcraft --out page.png --scale 2
echo '{"run": "insert.newPage", "params": {"name": "Q3"}}' | metriccraft-cli script Sales/ - --save
```
