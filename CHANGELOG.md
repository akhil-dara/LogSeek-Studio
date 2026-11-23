# Changelog

What changed in each release of LogSeek Studio. The newest release is first.

## 0.1.2

Unusual rows is easier to read on wide files.

- **The reason beside each row is one short line**: a value the row has, how many rows share it, and how many more reasons there are. A long value shows its beginning with the rest counted, and is never drawn over other rows.
- **A run of rows with the same reason says it once**; the rows under it read "the same as above".
- **The column of reasons folds away** from its heading, so the other columns have the room.
- **Row view shows the strongest reasons first**, in two columns, with the full list one click away. What a row shares with many others, such as cells empty in a large part of the file, is folded into one line.
- **Reasons can be copied**: a button in Row view, or a right-click on a row's line.
- **Row view follows the list**: when the rows are replaced by another list, it moves to a row that is in it.
- The Unusual rows sheet explains itself in fewer words, with the columns that were not counted folded away.

### Downloads

- `LogSeek-Studio-0.1.2-setup.exe`: the installer, for your own account or for all users.
- `LogSeek-Studio-0.1.2-windows-x64.zip`: the program alone; unzip and run.
- `SHA256SUMS.txt`: the checksum of each file.

The programs are not code-signed yet, so Windows SmartScreen may ask before the first run.

## 0.1.1

Fixes to reading unusual files.

- **A single line too long to hold** (for example one 120 MB line in a JSON lines file) no longer stops the import. That one record is left out and counted above the rows, and every row after it is read. Such a file also imports far faster than before.
- **A single list element too long to hold** in a JSON list is left out and counted in the same way, and memory stays within its budget while it is read past.
- **A record written over several lines among one-line records** (a pretty-printed record inside a JSON lines file) is read as one row. Before, each of its lines was counted as a row that was not valid JSON, which inflated the row count.
- **A large JSON file that is one object holding several lists** is now opened part by part whatever its size. The parts are found in the background while the window stays responsive.
- **Records back to back** in a `.json` file are read as rows, never taken for a file of parts.

### Downloads

- `LogSeek-Studio-0.1.1-setup.exe`: the installer, for your own account or for all users.
- `LogSeek-Studio-0.1.1-windows-x64.zip`: the program alone; unzip and run.
- `SHA256SUMS.txt`: the checksum of each file.

The programs are not code-signed yet, so Windows SmartScreen may ask before the first run.

## 0.1.0

The first public release.

### Opens

- CSV, TSV and other delimited files, and plain-text logs, with or without a header row.
- JSON: one record per line, arrays, records inside a wrapper key, pretty-printed JSON. Nested fields become columns.
- A JSON file that is one object holding several lists (a profile, an export with its sections) opens part by part: each list is a tab of its own, as a workbook's sheets are.
- Workbooks (`.xlsx`, `.xlsm`, `.xlsb`, `.xls`, `.ods`), Windows event logs (`.evtx`), and `.gz` and `.zip` files.
- Several files as one view, and cases that keep the files of one investigation together. The case sheet says whether the open files are in the case and adds them in one click.
- Many files open at once: tabs named by what tells them apart, kept to two scrolling lines, and a list of every open file with a find box.
- A file opened from inside a zip or a workbook says where that zip or workbook is, and the rest of what is inside it is one click away.
- A **Check before opening** sheet shows how a file will be read and lets you pick the timeline before anything is indexed.

### Search, filter and time

- Plain words, `column=value` terms, comparisons, value lists and time ranges; each filter is a chip that says what it does.
- **Build a search** from menus, **Search a whole list** of keywords, **Find**, and **Search every cell**.
- A time spine beside the rows, a calendar for days and hours, exact start and end times to the second, and quiet gaps marked between rows.
- Columns of times are asked about in time: on or after, before, between, with a calendar to pick the day from and a clock for the time.
- The row you are on stays selected and in view when a filter is removed or the rows are sorted.
- What you searched for is marked in the rows. A value too long for its cell starts at the match, and a match further right in a row is one click away. **What each one keeps** counts every condition of a search on its own, so the one that narrows it, or empties it, is plain. **Where it is** beside the search lists every cell that holds each searched word, by column, with one click to each.
- Saved searches, a query workspace, undo and redo.

### Columns

- A smart view of the most telling columns, with every column a click away; pin, wrap, fit, sort, reorder and group.
- **Add a column** from cards (split, combine, clean up, classify, date and time part, decode, maths, length and randomness, read as time) with a live result, and a formula language behind them.
- **Take from JSON**: a column whose values are JSON text gives up any field inside it as a column of its own, picked from the fields the rows really hold.
- Columns you made are listed in one place, each with what it is made from and a way to remove it.
- **Show values as**: read a column of numbers as times (Unix seconds to nanoseconds, Windows FILETIME, WebKit, Apple Cocoa, spreadsheet day numbers), or show a time column exactly as written.
- Every column list has a find box.

### Analysis

- Follow related rows from a cell's menu: one click to the rows that hold its value in a linked column, or the whole tree, listed in the order the rows are sorted by.
- Summarise by any number of columns, group rows in place, unusual rows with reasons, what sets rows apart, what is new in a period, timeline signals, patterns, shapes, strange text, rare pairs, and following related rows.

### Evidence and export

- Tags `1` to `9` with names, colour rules for rows and cells, and a row view of every field.
- Export rows as CSV, JSON lines or an Excel workbook; copy cells, blocks, rows and summaries.
- The file you open is only read. What could not be read cleanly is counted and said above the rows.

### Downloads

- `LogSeek-Studio-0.1.0-setup.exe`: the installer, for your own account or for all users.
- `LogSeek-Studio-0.1.0-windows-x64.zip`: the program alone; unzip and run.
- `SHA256SUMS.txt`: the checksum of each file.

The programs are not code-signed yet, so Windows SmartScreen may ask before the first run.

### Thanks

LogSeek Studio started from a question that kept coming up in brainstorming with [Sujay Adkesar](https://github.com/sujayadkesar): why struggle to open a large CSV or JSON file in Excel? A tool like this is something we had all wanted for a long time. Thanks to him for helping shape the idea.
