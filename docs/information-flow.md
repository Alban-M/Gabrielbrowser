# Information flow

Two invariants govern Gabriel. The first is implemented and tested. The second
is written down before the subsystem that will need it exists, because it is
much cheaper to design an egress boundary than to retrofit one.

> **1. No secret leaves the process.**
> Every output surface — terminal, JUnit XML, HTML reports, generated curl
> commands, error messages, panic messages, the support bundle, the vault file
> on disk — is asserted against the same canaries by `gabriel-testkit`.
> `gabriel har export` is the single documented exception.

> **2. No secret leaves the machine without explicit user approval.**
> Every egress path justifies itself against this rule: cloud models, crash
> reporting, update checks, telemetry, diagnostics, plugins, and any future
> collaboration feature.

The symmetry is the point. Today the redactor sits between the engine and the
output. Tomorrow it sits between the workbench and the model. Same philosophy,
opposite direction.

```text
today                          tomorrow
  engine                         workbench
    ↓                              ↓
  redactor                       redactor
    ↓                              ↓
  output                         model
```

## What leaves the machine today

Audited, not assumed. There is **no telemetry, no update check, and no crash
reporting endpoint** — every hardcoded host in the shipped crates is a test
domain or loopback.

| Path | Triggered by | Goes where | Carries secrets |
| --- | --- | --- | --- |
| The request being replayed | `gabriel run` | Wherever the request points | Yes — that is the request |
| OAuth token exchange | `gabriel auth` | The provider's token endpoint | Yes, necessarily |
| Browser handoff | `gabriel auth` | The provider's consent screen | No — the URL carries a challenge, not a secret |
| WebSocket frames | `gabriel ws` | Wherever the socket points | Yes, if the user sends them |
| Proxied traffic | `gabriel capture start` | The origin the client chose | Pass-through |
| Archive + checksum download | `install.sh` | The releases repository | No |
| Latest-version lookup | `install.sh` | `api.github.com` | No |

Everything else is local by construction. `gabriel doctor` binds a port and
touches the filesystem; it opens no connection. `gabriel jwt` decodes locally —
that is the reason it exists. `gabriel feedback` writes a directory and
transmits nothing.

One thing worth knowing: `gabriel init` seeds `base_url = "https://httpbin.org"`
in the example environment, so the starter request points at a third party. It
is only sent if someone runs it.

## The AI layer changes the shape of the problem

Every path above sends data somewhere the **user chose** — the API they are
debugging, their own identity provider. An AI feature sends data somewhere
*Gabriel* chose, on the user's behalf, and the payload is assembled from
whatever context makes the answer good. That inverts the incentive: the quality
of the feature pulls toward including more, and the invariant pulls toward
including less.

So prompt construction is a security boundary, and it should be a subsystem
rather than a habit:

```text
editor → context builder → secret classifier → redaction policy
       → prompt builder → model router → LLM
       → response validator → workbench
                ▲
                └── the boundary. Everything above it is local.
```

Two properties this shape buys that an inline approach does not:

- **One place to test.** The prompt builder gets the same treatment every other
  output surface got: canaries in, `assert_no_secret` on what comes out. That
  test is impossible to write if prompts are assembled at each call site.
- **A response validator.** Output from a model is server-controlled text, which
  Gabriel already knows how to distrust — it must be escape-defused before
  printing, and it must never be executed or replayed without the user seeing
  it first. A model that suggests a request is proposing an action, not taking
  one.

## Trust levels

Every AI action declares the level it needs. The level is the review unit: a
feature that wants a higher level has to argue for it once, in the open,
instead of quietly widening what a prompt includes.

| Level | What may be sent | Approval |
| --- | --- | --- |
| 0 | Documentation, schemas, Gabriel's own help text | None — nothing user-specific |
| 1 | Request *structure*: method, URL shape, header and field names, status codes. No values | Once, per workspace |
| 2 | Level 1 plus header and body values with secrets removed by the same redactor the terminal uses | Once, per workspace |
| 3 | Full bodies verbatim | Per action, showing exactly what will be sent |
| 4 | Anything, including secrets | Local model only — nothing leaves the machine |

Notes that matter more than the table:

- **Level 2 is not "safe".** The redactor removes values it *knows* are secrets.
  A response body can contain a customer's personal data that is nobody's
  credential, and no redactor will catch it. Level 2 means "no known secret",
  not "nothing sensitive".
- **Level 3 must show the payload, not describe it.** "Send the full request?"
  is not approval; a diff of exactly what will leave the machine is.
- **Level 4 is the only level compatible with a strict reading of invariant 2**,
  which is a reason to make local models a first-class path rather than a
  fallback for the privacy-conscious.
- The default for a new feature is **Level 1**. Raising it is a decision with a
  reviewer, not a default.

## Mapping the planned features

From the workbench plan, a first pass at what each would need:

| Feature | Level | Why |
| --- | --- | --- |
| Explain a request | 1 | Shape is enough to explain intent |
| Generate assertions | 1 | Assertions are about structure and status |
| Detect authentication issues | 2 | Needs to see header *forms*, not their values |
| Summarise API behaviour | 2 | Needs response shapes across several calls |
| Suggest mocks | 2 | Needs realistic shapes, not real values |
| Generate replay scenarios | 1 | Sequencing, not content |
| Security review | 3 or 4 | Reviewing what is *in* a body means seeing it |

The last row is the interesting one: the feature most in keeping with Gabriel's
identity is the one that needs the most data. That is the case for local models
being on the roadmap early rather than late.

## Settle before building

1. Does invariant 2 permit a **default-on** cloud feature at Level 1, or is
   first use always a prompt? (Recommendation: always a prompt. The first time
   anything leaves the machine is the moment trust is won or lost.)
2. Is approval remembered per workspace, per collection, or per session?
3. What does the workbench show *after* the fact — is there a log of what was
   sent, and can a user read it the way they can read a feedback bundle?
4. Does a captured request marked as coming from a production host get a lower
   ceiling than one from staging?
5. What happens offline, or when the user has approved nothing? Every AI
   feature needs a defined answer, and "greyed out" is a design decision worth
   making deliberately.

None of these are answerable by code, and all of them are cheaper to answer now
than after the first feature ships.
