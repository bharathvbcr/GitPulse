# Shared contracts

Three products — **GitPulse**, **DevCouncil**, **Manvi** — share five
artifacts. Each artifact has exactly **one canonical owner**. Everyone else
links, execs, or speaks the versioned contract in this directory.

This directory is the source of truth. GitPulse and Manvi carry *vendored
copies*, and each repo's CI fails if its copy has drifted.

---

## Ownership

| Artifact | Canonical owner | Contract here |
|---|---|---|
| Policy verdict | **Manvi** — `manvi/policy/decision.go`, `manvi/gate/gate.go` | `verdict.schema.json`, `verdict.cases.json` |
| Ledger event | **GitPulse** — it owns the resident watcher and the UI | `event.schema.json` |
| Task + lease store | **Manvi** — `crates/dc-store/src/schema.rs` owns the DDL | `lease.schema.md` |
| Code graph | **DevCouncil** — `rust-port` `devmap` | *(crate API, no wire contract)* |
| Verification gates | **Manvi** — `crates/dc-verify` | *(binary CLI, no wire contract)* |

Owning an artifact means: the owner's source is authoritative, the owner's
change lands first, and every consumer's copy is checked against it by a test
that reads the owner's actual source — never a transcription of it.

---

## Versioning

- Schemas are **versioned and additive-only**. Within v1 you may add an
  optional field. You may not remove a field, narrow a type, add a required
  field, or change what an existing value means.
- A breaking change requires a **new version file** (`verdict.schema.v2.json`)
  consumed side by side. Consumers migrate independently; the old version is
  removed only when no consumer reads it.
- `schema_version` travels **inside** the payload for ledger events, so a
  reader that does not recognise a version can degrade rather than guess.

### What "degrade" means here

A consumer that meets a value it does not recognise must render it as
*unknown*, never as the nearest known value, and never silently drop it. The
one rule this whole directory exists to protect:

> **A check that could not run must never render the same as a check that ran
> and passed.**

It has a mirror, found while validating this contract, that matters just as
much:

> **A check that ran and *failed* must never render as one that passed.**

Both are why `verdict.schema.json` carries `grant_id`, `demoted`, `widened`
and `degraded` as separate fields rather than collapsing them into `action`.
An `allow` says only that the operation may proceed. It does not say the rules
passed.

---

## Redaction

`argv_json` and `detail_json` on a ledger event carry command lines and
per-action payloads. Both are **redacted at write time, before insert** —
never at display time.

Display-time redaction protects the screen and nothing else: the secret is
already on disk, in a file that gets backed up, synced, and read by every
future consumer including ones that do not know to redact. Write-time
redaction is the only kind that bounds the blast radius.

The credential patterns are Manvi's, in `crates/dc-verify/src/rigor.rs`. They
are reused rather than reimplemented, for the ordinary reason: two copies of a
secret-detection regex means one of them is out of date and nobody knows which.

---

## Layout

```
contracts/
  README.md                        this file
  verdict.schema.json              the wire shape of a policy decision
  verdict.cases.json               generated parity fixture, 65 cases
  event.schema.json                the ledger event
  lease.schema.md                  state.sqlite tables, as implemented
  CHECKSUMS                        sha256 of each contract file
  tools/
    generate_verdict_cases.py      regenerates verdict.cases.json
    checksums.py                   writes/verifies CHECKSUMS
```

## Vendoring

GitPulse and Manvi each carry a copy under `contracts/`. To update a consumer
after changing a contract here:

```bash
python3 contracts/tools/checksums.py --write
```

then copy the directory into each consumer and run its contract tests. Each
consumer verifies two things independently:

1. **Its vendored copy matches `CHECKSUMS`** — nobody edited the local copy.
2. **Its own serializer matches the contract** — derived from that repo's type
   system, not from a hand-written list.

The second check is the one that catches real drift. The first only catches
someone editing a vendored file instead of the source.
