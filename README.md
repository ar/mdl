# mdl

Ledgers as Markdown tables. Each account is a `.md` file whose first 5-column table is
`| date | description | debit | credit | balance |`; `mdl` edits it from the terminal,
checks it, and prints it.

```
cargo install --path .
mdl init caja Caja       # a new account, caja.md, with the table template
mdl caja.md              # show the statement
mdl caja.md print        # typeset it as caja.pdf
```

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
