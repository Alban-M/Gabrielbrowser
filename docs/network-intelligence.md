# Evidence you can run

What Gabriel is, what it accepts, what it produces, and the one constraint that
decides how far it goes.

Written the day CI first went green, before the first tag, with 11,687 lines of
shipped code operating entirely at layer 7 and no user outside the machine that
wrote it. Numbers that read like measurements were measured; everything else is
a hypothesis, and §9 says which.

---

## 1. The north star

> **Evidence you can run.**

**Evidence**: what happened, faithfully — not what someone described.
**Run**: not a record of it. The thing itself, again.

Every word after this is an elaboration of those four.

A note on the category name, because a longer one was proposed and is worth
declining: *engineering intelligence platform* is abstract, verbless, and
indistinguishable from a hundred other vendors. It is the same passive
construction as *the memory layer for software*, rejected earlier for the same
reason. A category is remembered because it contains an action. "Evidence you
can run" is the category. Nothing goes above it.

---

## 2. Promotion is the company

One operation distinguishes Gabriel from everything adjacent to it, and it is
not capture, replay, or explanation. It is **promotion**: turning something that
was observed into something *kept, named, versioned and runnable* — with the
credentials removed.

```text
observed → reviewed → named → promoted → versioned → runnable → shared
```

That lifecycle is the product. `git commit` is the closest analogy: the value of
a commit is not that bytes were stored, it is that a human decided this state
was worth keeping and gave it a name — and history, diff, blame, revert and
review all follow from that one deliberate act.

**Why it needs to exist.** Engineering knowledge is produced continuously and
almost none of it survives the week. An incident is understood, a deployment is
debugged, a contract is figured out from a trace at eleven at night — and then
the tab closes. The understanding was real; nothing kept it in a form anyone
could use. Promotion is the moment that stops: it converts a temporary
investigation into a permanent capability, and it does so only for the things
someone judged worth keeping.

Promotion is the same act for behaviour. Two consequences:

- **Curation beats accumulation.** A store of everything the proxy ever saw is a
  landfill with a search box. A promoted request is a behaviour someone decided
  mattered, in a form that still runs.
- **Everything else composes onto it.** Diff, tests, OpenAPI, load scripts,
  documentation, incident reports — each is a transformation of a promoted
  artifact rather than a separate feature.

Today promotion applies to a single request. That is the smallest version of it.

The full lifecycle is also the roadmap, which makes feature arguments short —
every proposal has to name the stage it improves:

| Stage | Today |
| --- | --- |
| observed | capture proxy, HAR and PCAP import |
| reviewed | the promotion preview: what changes, and which rule changed it |
| promoted | **shipped** — a committable, credential-free file |
| named | request ids; behaviour names are next |
| versioned | git, because the artifact is a file |
| shared | git; a team surface is later |
| compared | `gabriel diff` |
| trusted | run it and see — the point of it being executable |

A proposal that improves no stage is not a Gabriel feature, however good it is.

---

## 3. The unit is a behaviour, not a request

Engineers do not think in requests. They think in *log in*, *check out*, *issue
a refund*. One of those is four hundred requests, some traces, a queue message
and a database write — but the name in someone's head is the behaviour.

So the promotable unit should be a behaviour: **a named, ordered set of
operations that happened together, kept as a unit.** Concretely a span of the
timeline with a name — not a new abstraction, a bigger selection.

What it buys: a vocabulary that matches how failures are actually reported
("checkout is broken", never "request 4,812 returned 422"), an artifact worth
handing to a colleague, and a diff that means something to a human — *checkout
changed* rather than *this header changed*.

What it must not claim: to be organisational knowledge. A named set of
operations is a fact, verifiable by running it. "Knowledge" is where products
stop being falsifiable, and an unfalsifiable claim is the thing this codebase
has spent its history removing.

---

## 4. What Gabriel accepts

Evidence from anywhere. If something records what a system did, it is input.

```text
runnable evidence          contextual evidence         promoted behaviour
───────────────────        ───────────────────         ──────────────────
HAR · PCAP · proxy         traces · metrics · logs     named · versioned
capture · DevTools         events · alerts · CI runs   credential-free
export · cURL · OpenAPI                                runnable · shared

contains the bytes         describes what happened     someone chose to keep it
        │                           │                           ▲
        └───────── re-run ──────────┴──── correlate ────────────┘
                              │
                              ▼
        tests · OpenAPI · load scripts · docs · incident reports
```

The three tiers are an architectural distinction, not a presentation.

**Runnable evidence** contains the actual bytes. It can be replayed.

**Contextual evidence** — an OpenTelemetry span, a Datadog export, a Kubernetes
event — is a *description*: sampled, body-less, often lossy. There is nothing in
it to send. It is valuable for correlation (*this trace points at that captured
request; here it is, running again*) and must never be presented as replayable.
Understanding without execution is the passive category the north star exists to
avoid.

**Promoted behaviour** is the third tier and the only one Gabriel creates rather
than consumes. It is what evidence becomes once a human decides it matters:
named, credential-free, committed, and still runnable a year later. It is the
asset the other two tiers exist to produce.

---

## 5. What Gabriel produces

| From | Gabriel produces | Status |
| --- | --- | --- |
| A captured request | a committable file that replays, credentials removed | **shipped** |
| A HAR from a colleague | the same, from their machine's evidence | **shipped** |
| Two runs of one behaviour | a diff that says what changed | **shipped** |
| A span of traffic | a named behaviour: ordered, session-sharing, runnable | designed |
| A PCAP from the network team | the conversation inside it, runnable | near |
| A behaviour | OpenAPI, k6, Playwright, JMeter | adjacent |
| A failed run | an incident report with the evidence attached | adjacent |
| A behaviour run a month apart | what changed in the contract, and when | future |

Three rows exist today. They are the rows the others are built on.

---

## 6. The constraint: replay is bounded by side effects

This decides how far the behaviour framing can go, and it gets *sharper* as the
framing gets broader — because **abstraction hides side effects**.

`POST /payments` visibly does something. "Issue a refund" does not visibly do
anything; it reads like a description. The vocabulary that makes behaviours
comprehensible is the same vocabulary that makes them look safe to re-run.

Replaying a `GET` is free. Replaying a payment charges the card twice. And the
behaviours most worth naming are the ones most dangerous to repeat:

| Behaviour | Replaying it means |
| --- | --- |
| Log in | a new session — safe |
| Search | safe |
| Check out | **a second order** |
| Issue a refund | **a second refund** |
| Publish a queue message | **downstream systems act again** |
| Deploy a release | **a deployment** |
| Rotate a secret | **the previous rotation is now wrong** |
| Provision infrastructure | **a second environment, and a bill** |

So the safety model comes before the breadth:

- **Classify every operation** — read, idempotent write, non-idempotent — from
  protocol and method semantics where knowable, and **explicitly unknown** where
  not. The line that matters is *changes state* rather than *is repeatable*:
  idempotence describes the second call, and the first `DELETE` still deletes.
- **A behaviour inherits the most dangerous classification it contains.** One
  non-idempotent request makes the whole behaviour non-idempotent, or a
  safe-sounding name conceals a charge. This is the replay counterpart to the
  information-flow invariants and belongs in code once replay widens — not in
  [information-flow.md](information-flow.md), which governs what leaves the
  machine rather than what gets performed on it.
- **Dry run is the default** for anything unclassified: resolve everything, send
  nothing, show exactly what would have gone.
- **Non-idempotent replays require confirmation showing what will be
  performed** — the build → show → approve → execute discipline already applied
  to AI payloads and installer checksums, applied to actions instead of data.
- **Some things are understood but never run.** A behaviour whose operations
  cannot be classified is still worth capturing, naming and diffing. Running it
  is not on offer.

Not a limitation to apologise for. It is the difference between a tool someone
trusts against staging and one they will point at production, and the second is
where this becomes a business.

---

## 7. Intelligence, applied where it belongs

```text
evidence → understanding → replay → verification → improvement
```

Most of what gets called AI in this space is not AI. Retransmissions, duplicate
ACKs, MTU discovery, certificate expiry, DNS failure, schema drift — all
**deterministic analyses**. Implementing them as prompts makes them slower,
costlier, non-reproducible and occasionally wrong. Ship them as code; let a
model explain the result to whoever is not a network engineer.

The strong application is **transformation**: a behaviour in, an OpenAPI
document or k6 script or Playwright test out. It needs no values (Level 1 under
[information-flow.md](information-flow.md)), the output is checkable by running
it, and it is promotion applied again.

All of it stays inside the four invariants: declared trust level, payload bound
by hash at approval, an egress record written before the send, and a
reproducible decision.

---

## 8. Who this is for

| Persona | The moment | What they have today |
| --- | --- | --- |
| **Backend / API developer** | "It worked yesterday" | Yesterday's bytes, unrunnable |
| **QA engineer** | Turning a real flow into a regression test | Tests encoding assumptions, not behaviour |
| **Support engineer** | Reproducing a customer's failure | A HAR that is inert |

The third is the most undervalued: their core task *is* Gabriel's core
operation, and they sit inside companies with budget.

A product serving twenty personas serves none of them first.

---

## 9. Sequence, gated on evidence

**Now.** Tag the preview. Five developers. One question: does the promotion
moment land without being explained? Nothing below starts before that answers —
if it does not land, none of this helps; if it does, the rest is ordering.

**Then.** The safety model in §6, before more breadth. Behaviours as the
promotable unit. gRPC and GraphQL replay, OpenAPI generation, PCAP import.

**If teams appear.** Shared behaviours, org policy on trust levels, egress
reporting — the last two are by-products of invariants 3 and 4.

This is also where the value stops being individual, and it is worth stating as
the hypothesis it is rather than the claim it is not: replay helps the person
who captured it, while a *promoted behaviour* is usable by someone who joins two
years later and never saw the incident. If that holds, the enterprise story is
not replay at all — it is that a team stops re-deriving the same understanding.
Nobody has tested it.

**If organisations appear.** What changes at that scale is *time*: behaviours
compared across months, so a contract change has a date and a cause. That
extends diff. Deliberately not a knowledge graph, which is where products go to
stop being falsifiable.

**Not designed here**, named rather than faked: screen specifications, a
component library, financials, a plugin SDK, compliance roadmaps. Each is
unblocked by the same thing — somebody using the product.

---

## 10. Where this goes

Engineering produces evidence every second. Almost all of it is discarded,
duplicated, or forgotten — read once during an incident and never again.

Gabriel turns that evidence into behaviours a team can keep: understood,
trusted, credential-free, and runnable a year later. Evidence stops being where
an investigation ends and becomes where the next improvement begins.

That yields a filter, which is the more useful half of a philosophy — it decides
what *not* to build:

> **Does this make engineering evidence more trustworthy, more understandable,
> more reusable, or more executable?**

If a proposal cannot answer that, it belongs in another product.

The first step is not a smaller version of that. It is the same claim at the
smallest scale that can be tested: a developer captures one request, promotes
it, and watches a file with no credential in it come back authenticated.

**Evidence you can run.**
