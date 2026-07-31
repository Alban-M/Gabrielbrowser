# Information flow

Four invariants govern Gabriel. The first is implemented and tested. The
others are written down before the subsystem that will need them exists,
because an egress boundary is much cheaper to design than to retrofit.

> **1. No secret leaves the process.**
> Every output surface — terminal, JUnit XML, HTML reports, generated curl
> commands, error messages, panic messages, the support bundle, the vault file
> on disk — is asserted against the same canaries by `gabriel-testkit`.
> `gabriel har export` is the single documented exception.

> **2. No secret leaves the machine without explicit user approval.**
> Every egress path justifies itself against this rule: cloud models, crash
> reporting, update checks, telemetry, diagnostics, plugins, and any future
> collaboration feature.

> **3. The user can always determine why a piece of information left the
> machine.**
> Every egress is recorded with its destination, its reason, and what it
> contained. Invariants 1 and 2 are claims; this is what makes them checkable
> by the person they protect.

> **4. Every information-flow *decision* is reproducible.**
> Given a retained record, Gabriel can say which feature asked, which trust
> level applied, which provider was chosen, which rule removed each excluded
> field, which payload hash was approved, and which response belongs to it.
> The decision is reproducible; the model's answer is not, and the invariant
> deliberately does not claim otherwise.

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

Invariant 3 is what turns the first two from promises into something a user
can verify. Invariant 4 is what makes the answer specific enough to act on:
"a token was removed" is reassurance, "the vault-value rule removed the
`Authorization` header" is an explanation. Both have mechanisms attached,
described below.

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

- **Level 2 is not "safe".** The redactor removes values it *knows* are
  secrets. Credentials are one class of sensitive data and not the largest:
  customer records, internal hostnames, proprietary code, regulated data and
  business documents are all invisible to a redactor, because sensitivity is a
  property of the organisation rather than of the string. Level 2 means "no
  known secret", not "nothing sensitive".
- **Level 3 must show the payload, not describe it.** "Send the full request?"
  is not approval; a diff of exactly what will leave the machine is.
- **Level 4 is the only level compatible with a strict reading of invariant 2**,
  which is a reason to make local models a first-class path rather than a
  fallback for the privacy-conscious.
- The default for a new feature is **Level 1**. Raising it is a decision with a
  reviewer, not a default.

## Approval is only meaningful if it binds

A review screen that describes a payload and a sender that assembles it
separately is theatre: a retry, a streamed follow-up, or a context builder that
runs again after approval can send something the user never saw. The two have
to be the same object.

So the flow is **build → hash → show → approve → send the approved bytes**, and
the sender refuses anything whose hash does not match what was approved. This
is the same discipline as the installer, which verifies a checksum before
copying rather than trusting that the download it validated is the one it
writes. Approval covers a payload, not an intention.

Two consequences worth designing for rather than discovering:

- **The record is written before the send, not after.** A crash mid-request
  must leave a trace; a log written on success only cannot answer "what did it
  send when it hung?"
- **The record is itself an output surface.** A prompt and a prompt log are
  two information flows, not one, so the allow-list, the canaries and the
  retention policy all apply to both. It should be readable the way a feedback
  bundle is readable — though whether it *ships* in one is a separate question,
  answered under Retention below.

What a user should be able to see, per request:

```text
destination   the model and provider, named
reason        the action that caused it — "generate assertions"
level         the trust level it declared
included      URL, method, JSON schema
excluded      Authorization, Cookie, vault secrets
payload       the exact bytes, on request
```

"Excluded" matters as much as "included": showing what was *removed* is what
demonstrates the redactor ran, and it is the difference between a user trusting
the claim and verifying it.

### What invariant 4 costs

Most of invariant 4 is a matter of writing enough down at the moment of the
decision. One clause is not, and it is worth naming the cost before agreeing
to it.

**"Which rule removed each excluded field"** cannot be answered by today's
redaction code. Both `scrub` and `Redactor::apply` have the shape
`&str -> String`: they take text, return text, and keep no record of which of
the four stages fired or what it matched. Attribution needs each rule to be
named and to report its own hits — closer to `&str -> (String, Vec<Removal>)`,
where a `Removal` carries the rule, the field, and the span.

That is a contained change and a worthwhile one, but it is a change to a
security-critical path that is currently simple and heavily tested. It should
be made deliberately, with the canaries extended to assert that a `Removal`
never carries the removed value itself — the record of a redaction is an
obvious place to accidentally reintroduce the thing that was redacted.

Until then, invariant 4 is satisfied at field granularity ("the `Authorization`
header was excluded") but not at rule granularity ("by the vault-value rule").
That distinction should be visible in the record rather than papered over.

## Retention

The record is audit evidence, and an audit log that cannot be deleted is
surveillance. Defaults:

- **Local only.** Never synchronised, never uploaded, no exceptions.
- **Encrypted at rest** if it persists across sessions — it describes what the
  user was working on, which is sensitive even when no value in it is.
- **Rotating**, by age or count, configurable.
- **Clearable in one action**, from the interface, without a confirmation maze.
- **Included in a feedback bundle only after explicit review.** This is stricter
  than the rule for everything else in the bundle, deliberately: the rest of a
  bundle is diagnostic, while this is a history of activity. A user who has
  already agreed to send diagnostics has not thereby agreed to send a record of
  what they worked on.

## Trust level before model

The provider interface splits on trust, not on vendor:

```text
local   — Ollama, llama.cpp, LM Studio, whatever comes next
cloud   — any hosted model
```

The workbench asks *what trust level does this action need* before it asks
*which model should answer it*. A Level 4 action has exactly one kind of
provider available, and that is a property of the action rather than a
preference in a settings screen. Routing by vendor first makes the privacy
question a configuration detail; routing by level first makes it structural.

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
3. How long is the egress record kept, and who can clear it? A record the
   user cannot delete is surveillance; one that rotates too quickly cannot
   answer "what did it send last Tuesday".
4. Does a captured request marked as coming from a production host get a lower
   ceiling than one from staging?
5. What happens offline, or when the user has approved nothing? Every AI
   feature needs a defined answer, and "greyed out" is a design decision worth
   making deliberately.

None of these are answerable by code, and all of them are cheaper to answer now
than after the first feature ships.
