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
- **Dynamic atoms.** How many of a busy worker's atoms are image atoms, and how many are dynamic (host map keys), is
  not measured yet. Count them before phase 2: only image atoms are shared.
