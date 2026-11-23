LogSeek Studio
==============

A fast, native log and table explorer for Windows. Open a CSV, JSON, Excel,
EVTX or plain-text log, then search, filter, group and compare it. Your file
is only ever read, and the program never uses the network.

First five minutes
------------------

1. Start LogSeek Studio and press Ctrl+O, or drop a file on the window.
2. On the "Check before opening" sheet, read how the file will be read, pick
   the timeline (or "No timeline") and look at the first rows. Press Enter.
3. Click in the search box (or press /) and type a word. Rows that contain it
   stay. Press Esc to leave the box.
4. Press Space to see one row as a table. Press 1 to 9 to tag a row.
5. Press Ctrl+K and type what you want to do, or a column name. Press ? for
   the list of keys.

Where things are kept
---------------------

LogSeek builds an index for each file you open. Indexes are kept in
%LOCALAPPDATA%\LogSeek\cases unless you choose another folder with
"Keep indexes in another folder" on the start screen. Removing that folder
removes every index, tag and made column; your own files are not touched.

Documentation, source and new versions
--------------------------------------

https://github.com/akhil-dara/LogSeek-Studio

Licence
-------

LogSeek Studio is free software under the GNU Affero General Public License,
version 3 or (at your option) any later version. It comes without any
warranty. The full text is in LICENSE.txt. The libraries it is built on, and
their licences, are in THIRD-PARTY-LICENSES.txt.
