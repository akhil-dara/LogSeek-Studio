# Limits and known gaps

This page lists what LogSeek Studio does not do yet, and the places where it works within a limit. It is meant to save you time: if something here matches what you were about to try, use the workaround. Each item says what happens today.

## Platform

- LogSeek Studio is a Windows desktop app. Build and use it on Windows 10 or 11.
- The smallest window is 960 x 600 pixels. The layout is designed for 1366 x 768 and up.

## Searching and filtering

- **Typing a column filter.** The filter words (`^=`, `$=`, `5~10`, `IN (...)` and the rest) belong to column filters. You do not type them in the search box. In the search box, `~` means a regular expression. Use a column's funnel and its **Condition** tab to write them for you.
- **Counts and groups are not written in the search box.** The search box accepts `| where`, `| sort` and `| fields`. The steps `stats`, `top`, `timechart` and `head` are understood but refused with a pointer to **Summarise**.
- **Select all with a search** in a value list acts on the matches loaded so far, not on every match.
- **Find** steps from row to row, not from cell to cell. While the rows are sorted, it shows the total only.

## Columns and formulas

- **You cannot type a formula.** Made columns are built from forms, **Convert** items and field picks. LogSeek shows the formula it made, on hover, but there is no place to write one.
- **Group by a made column** is in the menu but greyed out.
- **The JSON or XML banner** over a column lists the first three suggested field names and then "and N more". Every field stays one click away in **Choose columns**.

## Analysis

- **Grouping.** There is no activity strip per group yet. Right-clicking a cell inside a grouped row does not act yet.
- **Sorting groups.** A grouping level with tens of millions of different values holds the group names in memory while it sorts. It should sort on disk, and does not yet.
- **Summarise** can reach its memory limit when it counts the different values of a column that has a very large number of them. It then tells you the counts shown are at least these.
- **Follow related rows.**
  - It does not follow across files.
  - It does not follow on two column pairs at once.
  - It does not follow by a made column.
  - The result tree has no tag marks and no colour rules.
  - On very large files each step is one full pass over the file.

## Selecting and copying

- **Row selection** works in the plain list. It does not work in the grouped view or in the Follow tree. There, `Ctrl+C` copies the focused cell only.
- **Large copies are refused.** If the text to copy is too big for the clipboard, LogSeek says so and points you to **Export rows**.

## Opening files

- **Workbooks.** Formula cells show the result saved in the workbook. LogSeek does not recalculate them, and it does not show the formulas themselves. Cell colours, merged cells and number formats other than dates are not carried over. The workbook's text table is held in memory while a sheet is read.
- **Old indexes.** An index made before a newer way of reading was added does not pick it up. Use **Read the file again** in **Start from scratch**. Tags and made columns stay.

## Keyboard and mouse

Some gestures have no keyboard path yet:

- the time spine's period brush (drag down to keep a period);
- the calendar's hour strip.

The right-click menus of cells and column names can be opened from the keyboard with `Shift+F10` and `Alt+Down`.

## Not built yet

These are planned and not in the app:

- One combined view across all the files of a case.
- A separate window per file.
- An entity view.
- Collapsing near-duplicate rows.

See [Keyboard](keyboard.md) for the keys that do exist.
