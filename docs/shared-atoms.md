# Shared atoms: a design

Status: proposed, not implemented. It needs a decision first, because it adds state that every runtime of the process
reads, where runtimes share nothing mutable today.

## Why

Every runtime interns its own atoms. A fresh runtime holds 513, the realm's names and the builtins' property names,
about 25 KB of string records and their text, and most of its 8 KB of tables. A busy mr-moqui service worker holds about
11k: its strings were 1.6 MB, mostly atoms, and its tables 1.1 MB, inline caches and atoms together (mr-moqui Profiling2
§3, R49). About 30 runtimes a process multiply this. Most of these atoms are the same texts in every runtime: builtin
names, and the keys, names and string constants of the code images that every thread loads.

## What is shared

Two kinds of atom become process-wide and immutable:

1. **Builtin atoms.** The realm's names (`Names`) and every builtin property and function name, fixed by the build.
   They are made once per process (a `OnceLock`) in a fixed order, so their ids are the same in every runtime and could
   be constants.
2. **Image atoms.** The atoms of a code image (`CodeImage::atoms`) go into an append-only process table when the image
   is first loaded, up to a cap on count and bytes. Past the cap, and for transient code, atoms stay per runtime.

Each runtime keeps its own atom table for dynamic atoms: keys computed at run time, map keys that came from the host,
and strings a script interns.

## Ids

`StringId` stays a `u32`, and `PropertyKey` and `Value` keep their encodings. Shared atoms take a high range of the
string index space, `[SHARED_BASE, KEY_INDEX_LIMIT)`, and a runtime's string arena stays below `SHARED_BASE`.
`Heap::string(id)` and the arena's other readers (about 120 sites, most through `Heap::string`) test `id >= SHARED_BASE`
and read the shared table instead. The test is one compare and predictable.

The shared table is a segmented array: segment `k` holds `2^k` records and is set once (`OnceLock`), so a reader finds a
record with no lock, and a record never moves. Inserting takes a mutex around a text-to-id map. This is safe Rust; the
workspace forbids `unsafe`.

`StringRecord` must be `Sync` to live in the shared table:
- its `hash: Cell<u32>` becomes an `AtomicU32`, read `Relaxed`;
- its `flat: OnceCell` becomes a `OnceLock`, whose `get` is one acquire load;
- or shared atoms get their own record type, at the cost of an enum in `Heap::string`'s result.

## One id per text in a runtime

Atoms compare by id, so a runtime must never hold two ids for one text. Interning looks in the runtime's table first,
then in the shared table; it adds a runtime atom only when both miss. A runtime that made a dynamic atom for a text
before another thread shared that text keeps using its own id. An image load maps each image atom the same way, so a
unit's key table can name a runtime atom where that runtime had one first. The per-unit key tables therefore stay; they
are built when the unit is loaded, as today.

## The collector

Shared atoms are immortal. Marking skips ids at or above `SHARED_BASE`, and the sweep never sees them. They need no
`AtomKeep`, and the atom modes (`Pin`, `Hold`, `Weak`) apply to runtime atoms only. There is no refcount: the fork's
atoms are traced, not counted as QuickJS's are. The shared table is bounded by the image store's cap (64 MiB of images
in mr-moqui) and its own count cap. The atoms of an image the process drops stay; they are bounded by the distinct texts
the process has loaded.

## Thread safety and the threading model

Runtimes stay `!Send` and single-threaded. What is new is one process-wide `Sync` table:
- read without a lock;
- written under a mutex when an image is first loaded somewhere in the process, or a builtin is made at the first
  runtime's start.

That is the change to the threading model. Today two runtimes share nothing mutable, and code images are immutable
`Arc`s.

## F1 step 3

Instructions are already shared: an image unit's instructions name a key by its index in the unit's key table, and
every runtime that loads the image shares the body (`image.rs`, F1 step 2). That is the "unit-local key table" choice
of F1 step 3. What stays per runtime is each unit's `CodeUnit`: the key table (`keys: Box<[PropertyKey]>`), the cache
base, the name atom and the children.

Shared atoms would make a unit's key table the same in every runtime whose atoms for those texts are the shared ones,
so it could live in the shared body, with a per-runtime table only where a runtime had a dynamic atom first. Moving the
cache base to a per-runtime table by unit id, F1 step 3's other part, is independent of this design. So F1 step 3 does
not follow on its own: shared key tables are a further step that shared atoms make possible.

## Phases and effort

| Phase | Work | Days |
| --- | --- | --- |
| 1. Builtin atoms | `Sync` string records; the fixed table and its id range; `Heap::string` and the other readers; marking, sweep, verify and the memory report; `Names` from the shared table; tests, gc-stress | 3 to 5 |
| 2. Image atoms | the segmented append-only table and its mutex; image loads and interning through it; the cap; tests with threads | 3 to 4 |
| 3. Shared key tables (optional) | key tables in the image body where every key is shared | 2 |
| Checks | Test262, the Sweep and memory runs, mr-moqui's heap runs (`MOQUI_SCRIPT_STATS_FILE`) | 2 |

That is about two weeks for phases 1, 2 and the checks, or 8 to 11 days of implementation. Phase 1 alone, about a
week, saves the 513 atoms of each runtime's floor. Phase 2 is the part that reaches the busy workers' 11k atoms.

Risks:
- **Every string read.** Each string read gains a branch: the interpreter's property paths and the string builtins.
  Measure it with the Sweep's instruction gate.
- **Dynamic atoms.** Only image atoms are shared. The counts below show they are most of a runtime's atoms.

## The counts

`Runtime::atom_census` (core) and `api::core::Runtime::atom_census` count a runtime's live atoms by class, each atom
in the first class that holds it:

1. builtin: the text is one of a fresh runtime's atoms;
2. image code: a key, name or atom of a unit loaded from a code image (its body is shared);
3. static code: the same for the other units in the runtime's code store, the code it compiled itself;
4. dynamic code: the same for the heap's run-time units;
5. constants: a string literal among a live function's constants;
6. object keys: a key of a live shape, the host's map keys among them;
7. other: held only as a value, or by nothing until the next collection.

With `QJS_ATOM_CENSUS` set, each memory report prints the census to stderr, so an embedder's runs can be counted with
no change on its side. `the_atom_census_classes_each_atom_once` in `tests/embedding_api.rs` tests it.

The counts below are from mr-moqui Round 50's `js_speed` on this branch (fork 5a601429). Each cell is atoms, with the
UTF-16 units of their text in parentheses.

| Runtime | Atoms | Builtin | Image code | Static code | Dynamic code | Constants | Object keys | Other |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| main thread, after the expression rows | 2022 | 512 | 0 | 940 | 0 | 170 | 206 | 194 |
| main thread, after every row (scripts, actions, services, templates, screens) | 4188 (54k) | 512 (4.6k) | 75 (0.4k) | 1361 (14.3k) | 394 (3.8k) | 1418 (26.1k) | 269 (3.4k) | 159 (1.5k) |
| a new thread, after its first script and screen | 3081 (35k) | 512 (4.6k) | 533 (4.6k) | 862 (9.8k) | 0 | 797 (12.0k) | 222 (2.8k) | 155 (1.5k) |

moqui-core's `many_distinct_units_and_texts_keep_the_heap_bounded` runs 1600 screen units, each with 80 distinct
string literals, and 2000 texts. After its collection: 59,749 atoms (1.23M units). Of these, 57,881 are constants
(1.21M units), 938 static code, 512 builtin, 221 object keys and 197 other.

What the counts say:
- **Code atoms are most of a runtime's atoms.** Builtin and code atoms (image, static, dynamic and constants) are 90%
  of the main thread's and 88% of a new thread's. The host's keys and the rest are 10 to 12%: about 400 atoms. A
  screen's string literals can dwarf everything else.
- **A new thread compiles much of its code itself.** Its 862 static-code atoms are its own compiles: the start's evals,
  the helpers and the shims. mr-moqui's C4 (the start through the code images) would make them image atoms.
- **The main thread is the one that compiles.** Its 1361 static-code atoms and its constants are what the other
  threads load as images.
- **Constants have to be shared too.** An image's string literals are atoms, interned at load (`CodeImage::atoms`), and
  they are the largest class whenever code carries text. Phase 2 must share them with the keys.
- **The 11k atoms of R49's busiest worker are not reproduced here.** By these classes, about 90% of them should be
  code atoms. Confirm that with `QJS_ATOM_CENSUS` on an mr-mantle run before phase 2.
