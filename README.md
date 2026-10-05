# mdl

Ledgers as Markdown tables. Each account is a `.md` file whose first 5-column table is
`| date | description | debit | credit | balance |`; `mdl` edits it from the terminal,
checks it, and prints it. A `|` in a description is written `\|` in the file, as GFM
reads it, so the table stays a table.

## Install

```
cargo install --git https://github.com/ar/mdl
```

Or from a checkout: `cargo install --path .`

```
mdl init caja Caja       # a new account, caja.md, with the table template
mdl caja.md              # show the statement
mdl caja.md print        # typeset it as caja.pdf
```

## Sample

[`cash.md`](cash.md) is a small Cash account with a month of entries, including a
note row, to try the commands on:

```
mdl cash                       # bordered table with totals (the .md is optional)
mdl cash --markdown            # the original Markdown table
mdl cash show 2026-09          # a monthly statement
mdl cash.md print --graph      # cash.pdf, with the balance charted
mdl cash.md show --graph       # statement with the same chart inline in Ghostty/Kitty
mdl cash.md graph -o cash.png  # save the chart as a PNG
```

The default display uses the interactive screen's table borders and totals, preserving the
account's column labels. Headers, totals, and flagged entries appear bold in a
terminal; redirected output has no terminal escape codes. Use `--markdown` for the
original Markdown output, `--json` for JSON, or `--csv` for a CSV with the account's
column labels as its header. `--pretty` explicitly selects the default format; these
output flags are mutually exclusive.

`mdl balance cash bank` (or `mdl cash bank balance`) shows each account's closing
balance and their total in a bordered table. Use `--markdown`, `--json`, or `--csv`
to export it, or `-q` / `--quiet` for just the amounts, one per line, with the total
last and no account names or headers. This also applies to `mdl cash balance`.
`--total` selects just the total; combine it with `-q` for a single numeric value.
These output formats are mutually exclusive, and flags can appear before or after
account names. JSON contains an `accounts` array of `account` / `balance` objects
and a numeric `total`; CSV has `Account,Balance` columns and a final `Total` row.

The display accepts the same periods as `print`, with or without `show`:

```
mdl cash last                   # the previous month
mdl cash last 3                 # the three months before this one
mdl cash this                   # the current month
mdl cash this 3                 # the current month and the two before it
mdl cash 2026-07 2026-09         # an inclusive month range
mdl cash show last 3 --markdown # the same period as a Markdown table
```

Each period starts with its opening balance; debit and credit totals cover only
entries within that period. Output flags may appear before or after the period.

## Interactive entry

Run `mdl` without arguments to open the interactive ledger, or `mdl edit cash` to
open it with a specific account loaded and the description field ready for entry.
If the file is missing, it is created with the default English ledger table and a
zero opening balance dated today. If the file exists without a ledger table, the
table is appended, preserving its text. Existing ledgers are opened as they are.
This session stays on that account: the account field shows only its filename
without `.md` and cannot be changed. Ctrl-L clears the entry while keeping the
account open. Run plain `mdl` to switch between accounts.

`mdl edit <file>` opens the local account without fetching and exits without
syncing. Use `mdl sync` or Ctrl-S when you want to sync. Plain `mdl` continues to
fetch on entry and sync pending work on exit.

The `.md` extension is optional. `mdl cash edit` remains an alias. The command
`mdl cash edit <row> <date> <debit|-> <credit|-> <description...>` still edits a row
directly from the command line.

The entry panel labels
the fields and shows whether you are adding, editing, or inserting a row. The active
field has a highlighted value and a cyan label; Enter advances or saves, and Tab
moves between fields. When editing a row, Shift-Tab from Description reaches Date. Date digits are
overwritten in place; Left/Right skip the fixed separators, and Home/End reach the
first/last digit. Backspace/Delete clear a digit for replacement.
Changing the date places the entry after existing rows on that day, recalculates
balances, and keeps the moved entry selected. Short terminals use a compact entry row.

On a selected statement row, `t` prepares a transfer counterpart: choose the
**destination account** with the usual autocomplete, then review the amount. The
new entry keeps the source date, swaps debit/credit, and appends `(from account)`
to the description, or `(de account)` when the destination table has a Spanish
`Fecha` header. The account name is the source filename without `.md`; the
description remains editable. The amount
is selected for replacement, so a different currency can use a converted amount or
an expression such as `150*40.50`. Enter saves in the destination and returns to the
source row; Esc cancels at either step. The source entry stays unchanged.

`r` prepares a reversal in the same account, with the same date, swapped debit/credit,
and the original description in parentheses. The description is focused first;
all fields remain editable. Enter advances and saves a separate entry; Esc cancels.
Both actions insert in date order and appear in session history. Rows without an
amount cannot be transferred or reversed.

Ctrl-H shows entries posted in the current TUI session as a table in the main screen,
newest first, with Date, Account, Description, Debit, and Credit columns. Sessions
opened with `mdl edit <file>` omit the Account column until transfers involve
another account. Up/Down, PgUp/PgDn, or the
mouse wheel scroll the history; Esc or Ctrl-H returns to your unchanged entry form.
History is read-only and includes new entries and inserted rows, including notes.
It disappears when the session ends.

The footer keeps keyboard hints separate from git status. A spinner marks a fetch,
save, or sync; green checkmarks show successful updates, amber marks pending pushes
or offline work, and red marks sync errors. Ctrl-S syncs with the remote. During a
save or manual sync, input waits until the operation finishes.

Git is disabled for accounts outside the working directory, outside its Git
repository, or reached through a symlink that points outside it. These accounts
save directly without fetching, committing, or syncing on exit. Git scope and
autocommit settings are checked when an account opens, not on each keystroke or
local save.

## New accounts

`mdl init <file[.md]> [--lang es|en] [title...]` writes the file with a heading, a line
of prose and the table, holding an opening row at 0.00 dated today. The labels are
English (`Date | Description | Debit | Credit | Balance`) unless `--lang es` asks for
Spanish (`Fecha | Descripción | Debe | Haber | Saldo`). The title defaults to the
file's stem.

```
mdl init bank Bank                  # bank.md, English labels
mdl init caja --lang es Caja chica  # caja.md, Spanish labels
mdl init notes/acu                  # acu.md exists: the table goes at its end
```

An existing file is never overwritten: when it has no table yet, `init` appends one
after a blank line and leaves everything else in place (the title is not used, since
the file has its own), so a file of free-form notes becomes an account that keeps them
as its prose. A file that already has a table is refused.

The labels are the file's own from then on: `mdl` reads the table by position, never
by wording, so any language works, and the balance column's label is what a period
statement's opening row is called.

## Notes

A row with neither a debit nor a credit is a note: a dated remark that leaves the
balance as it was, such as a reconciliation or a claim filed. `mdl <file> note
[--date YYYY-MM-DD] <description...>` appends one; in the interactive screen, Enter
through the empty amount fields adds one; `edit <row> <date> - - <description...>`
turns a row into one. Notes render with empty amount cells and count for nothing in
the totals.

```
mdl caja.md note Arqueo: coincide con el banco
mdl caja.md note --date 2026-09-19 Reclamo enviado
```

A `--date` before the last row, on `note`, `debit` or `credit`, inserts the entry in
date order, after the rows already on that day, and recomputes the balances from
there; `mdl` says which row it became.

## Computed amounts

An amount on the command line may be a computation: `+`, `-`, `*`, `/`, parentheses
and decimals, rounded to cents.

```
mdl cash.md debit 100+200+50 Three invoices
mdl cash.md credit 1000*40.50 Dollars bought
```

In the interactive screen, type calculations directly into Debit, Credit, or
Balance, for example `40.50*1000`, `(100+200)/3`, or `500-25.50`. Enter evaluates
and advances or saves as usual; Tab evaluates before changing fields. Typing a
second decimal digit does not submit, so you can continue the calculation. Invalid
calculations stay in the field with an error so you can correct them. Long
calculations scroll within the field.

Only the calculated amount is saved, rounded to cents. A calculation in Balance
sets the target balance and creates the debit or credit needed to reach it.
Descriptions are plain text; existing `#` descriptions remain unchanged and are
no longer evaluated or checked by `lint`.

## Printing to PDF

`mdl <file> print [--graph [balance|debit|credit]...] [period]` typesets the statement with [Typst](https://typst.app),
a single-binary, open-source (Apache 2.0) typesetter. It is the only external tool
`mdl` needs, and only for PDF or graph rendering. When the file has text outside the
ledger table, printing uses the closest `#` title above the table, includes the
Markdown between that title and the table and after the table, and ignores text
before the title. Without a title above the table, the filename is used as the title.
The surrounding Markdown is rendered by the pinned [cmarker](https://typst.app/universe/package/cmarker/)
Typst package, which Typst downloads on first use and caches for later prints.

### Install Typst

macOS (Homebrew):

```
brew install typst
```

Linux and Windows package managers:

```
sudo pacman -S typst                    # Arch
sudo apt install typst                  # Debian 13+ / Ubuntu 24.10+
nix-env -iA nixpkgs.typst               # Nix
winget install --id Typst.Typst         # Windows
scoop install typst                     # Windows
```

Any platform, prebuilt binary: download the archive for your system from
https://github.com/typst/typst/releases, unpack it, and put the `typst`
executable on your `PATH`.

Any platform, from source (needs a Rust toolchain):

```
cargo install --locked typst-cli
```

Check it:

```
typst --version
```

`mdl` looks for `typst` on the `PATH`; if it is missing, `print` and graph rendering say so.

### Fonts

The statement is set in Helvetica Neue when the system has it (macOS does) and falls
back to Libertinus Serif, which Typst bundles, elsewhere. Nothing to install.

### Usage

```
mdl caja.md print                     whole ledger           → caja.pdf
mdl caja.md print 2026-08             one month              → caja-2026-08.pdf
mdl caja.md print 2026-07 2026-08     inclusive range        → caja-2026-07-2026-08.pdf
mdl caja.md print last                the previous month
mdl caja.md print last 2              the two months before this one
mdl caja.md print this                the current month
mdl caja.md print this 2              the previous month and this one
mdl caja.md print --graph last 3      with the balance charted after the table
mdl bills.md print --graph debit      each entry's debit charted instead
mdl caja.md print --graph debit credit   both amounts, in two colours
```

A period statement opens with a row carrying the balance before it, and the totals
cover the period only. `show` accepts the same period words.

`--graph` adds a chart after the table: the balance stepping from entry to entry
across the period's months (or, without a period, the months from the first entry to
the last), the axis ticked by month, quarter or year as the page fits, the zero line
drawn, and the closing balance labelled. `--graph debit` or `--graph credit` charts
each entry's amount instead of the balance, for an account that records, say, bill
payments as single entries (the balance only ever grows, the amounts are the story);
both words chart both, each in its own colour. Typst draws
it from the ledger's own numbers, with nothing more to install.

### Inline and PNG graphs

`mdl <file> show --graph [balance|debit|credit]... [period]` displays the normal
statement followed by the PDF chart in Ghostty or Kitty. Other terminals still show
the statement. `mdl <file> graph [balance|debit|credit]... [period]` displays just the
chart; use `-o chart.png` to save it, including on terminals without inline graphics.
The series and period work the same way as `print --graph`. All three use the same
Typst chart definition and ledger data, so the graph's shape, colours, ticks and
labels stay aligned. When combining accounts, balance graphs plot each account's own
balance in a separate colour and their sum as a dark line. Inline
display is disabled inside tmux and screen.
