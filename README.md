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
```

The default display uses the interactive screen's table borders and totals, preserving the
account's column labels. Headers, totals, and flagged entries appear bold in a
terminal; redirected output has no terminal escape codes. Use `--markdown` for the
original Markdown output, `--json` for JSON, or `--csv` for a CSV with the account's
column labels as its header. `--pretty` explicitly selects the default format; these
output flags are mutually exclusive.

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

Run `mdl` without arguments to open the interactive ledger. The entry panel labels
the fields and shows whether you are adding, editing, or inserting a row. The active
field has a highlighted value and a cyan label; Enter advances or saves, and Tab
moves between fields. Short terminals use a compact entry row.

The footer keeps keyboard hints separate from git status. A spinner marks a fetch,
save, or sync; green checkmarks show successful updates, amber marks pending pushes
or offline work, and red marks sync errors. Ctrl-S syncs with the remote. During a
save or manual sync, input waits until the operation finishes.

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

A description may carry its own computation: one that starts with `#` followed by an
expression with at least one operator, such as `#1000*40.50 currency exchange` or
`#100+200+50 sundries`. Its value is the row's effect on the balance: positive is a
debit, negative (`#-1000*40.50 USD sale`) a credit. The expression stays in the
description, so the row shows where its amount came from. `lint` checks that each
such description agrees with its row, and in the interactive screen Enter from the
description fills the amount field with the value, selected, ready to accept with
another Enter or to type over. Without the `#` a description is prose however it
looks (`2-3 people`, `1000*40.50 exchange`), and so is `#123 invoice`: a `#` with just
a number.

```
mdl cash.md debit 40500 '#1000*40.50 currency exchange'   # lint: agrees
mdl cash.md debit 40000 '#1000*40.50 currency exchange'   # lint: description computes 40500.00 but the row is 40000.00
```

On the command line the `#` needs quoting, or the shell reads it as a comment.

## Printing to PDF

`mdl <file> print [--graph [balance|debit|credit]...] [period]` typesets the statement with [Typst](https://typst.app),
a single-binary, open-source (Apache 2.0) typesetter. It is the only external tool
`mdl` needs, and only for `print`.

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

`mdl` looks for `typst` on the `PATH`; if it is missing, `print` says so.

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
