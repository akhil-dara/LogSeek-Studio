# Third-party notices

LogSeek Studio is built on the following libraries. Each is distributed
under its own licence. The full list of every library built into the
program (195 in all, with versions, authors and licence texts) is in
[licenses/THIRD-PARTY-LICENSES.txt](licenses/THIRD-PARTY-LICENSES.txt), and
that file travels with the installer and the portable zip.

| Library | Used for | Author | Licence |
|---|---|---|---|
| [egui and eframe](https://github.com/emilk/egui) | The desktop interface: the window, drawing, text, input | Emil Ernerfeldt and contributors | MIT or Apache-2.0 |
| [winit](https://github.com/rust-windowing/winit), [glutin](https://github.com/rust-windowing/glutin), [glow](https://github.com/grovesNL/glow) | The window and OpenGL drawing under egui | The winit and glutin contributors; Joshua Groves, Dzmitry Malyshau | Apache-2.0; MIT, Apache-2.0 or Zlib |
| [Phosphor Icons](https://phosphoricons.com) through [egui-phosphor](https://github.com/amPerl/egui-phosphor) | The interface icons | Phosphor Icons; amPerl | MIT; MIT or Apache-2.0 |
| [rfd](https://github.com/PolyMeilex/rfd) | The system's open and save dialogs | Poly | MIT |
| [calamine](https://github.com/tafia/calamine) | Reading workbooks: `.xlsx`, `.xlsm`, `.xlsb`, `.xls`, `.ods` | Johann Tuffe | MIT |
| [evtx](https://github.com/omerbenamram/EVTX) | Reading Windows event logs (`.evtx`) | Omer Ben-Amram | MIT or Apache-2.0 |
| [zip](https://github.com/zip-rs/zip2) | Reading `.zip` files and writing Excel workbooks | Mathijs van de Nes, Marli Frost, Ryan Levick and contributors | MIT |
| [flate2](https://github.com/rust-lang/flate2-rs) | Reading `.gz` files | Alex Crichton, Josh Triplett | MIT or Apache-2.0 |
| [zstd](https://github.com/gyscos/zstd-rs) and [Zstandard](https://github.com/facebook/zstd) | Compressing the index | Alexandre Bury; Meta Platforms, Inc. | BSD-3-Clause; BSD-3-Clause |
| [regex](https://github.com/rust-lang/regex), [aho-corasick](https://github.com/BurntSushi/aho-corasick), [memchr](https://github.com/BurntSushi/memchr) | Searching text: patterns, many keywords at once, fast byte search | The Rust Project Developers; Andrew Gallant | MIT or Apache-2.0; Unlicense or MIT |
| [csv-core](https://github.com/BurntSushi/rust-csv) | Reading delimited text | Andrew Gallant | Unlicense or MIT |
| [serde](https://github.com/serde-rs/serde) and [serde_json](https://github.com/serde-rs/json) | Reading JSON, and saving settings and index records | Erick Tryzelaar, David Tolnay | MIT or Apache-2.0 |
| [crossbeam](https://github.com/crossbeam-rs/crossbeam) | Passing work between the window and the engine | The Crossbeam Project Developers | MIT or Apache-2.0 |
| [xxhash-rust](https://github.com/DoumanAsh/xxhash-rust) | Fast hashing of values | Douman | BSL-1.0 |
| [windows-sys](https://github.com/microsoft/windows-rs) | Calls to Windows | Microsoft | MIT or Apache-2.0 |

## Fonts

LogSeek Studio draws its text with the fonts already installed on the
machine (Segoe UI, Cascadia Mono, Consolas); it does not ship them. The
interface library carries fallback fonts inside the program, used for
characters the system fonts lack:

| Font | Licence |
|---|---|
| Ubuntu Light | Ubuntu Font Licence 1.0 |
| Hack | MIT, with the Bitstream Vera licence for the parts derived from it |
| Noto Emoji | SIL Open Font License 1.1 |
| emoji-icon-font | MIT |

## Licence compatibility

Every library above is under a permissive licence (MIT, Apache-2.0, BSD,
Zlib, ISC, BSL-1.0, Unlicense and the like), each of which allows use in a
program distributed under the GNU Affero General Public License, version 3
or later. Where a library offers a choice of licences, LogSeek Studio uses
it under one that is compatible with that licence.

## Keeping this up to date

The full list is generated, not written by hand:

```
python tools/third_party_licenses.py
```

It reads the dependency list from Cargo and the licence files of the
library sources Cargo has downloaded. Run it after any change to the
dependencies and commit the result.
