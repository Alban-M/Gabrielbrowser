# Gabriel: the product

Written before there are users, which makes most of it a hypothesis. The parts
that are measured say so; the parts that are opinion say so too. It is here to
be falsified by [Preview 1](preview-1.md), not to be admired.

---

## 1. What Gabriel actually is

Not "an API client with AI". The claim is narrower and harder to copy:

> **Gabriel turns what your application actually did into engineering assets you
> can trust — and can prove nothing leaked doing it.**

Three words carry the weight.

**Actually.** Every other tool in this space starts from a request you *describe*.
Gabriel starts from one that *happened*. A described request is a hypothesis
about your system; a captured one is evidence. That difference is not a feature,
it is the direction data flows, and it is why the promotion step exists.

**Trust.** A captured request is worthless if using it leaks a credential into a
repository. Gabriel's answer is not a warning label — it is that a promoted file
has no credential in it and replays anyway. Measured, not asserted.

**Prove.** Four invariants, tested on every output surface, with a documented
exception list. See [information-flow.md](information-flow.md).

### The category

"API testing" is a crowded category and the wrong one. The honest category is:

> **Evidence-based API engineering.**

The competitor is not Postman. It is the tab where a developer has a cURL
command they pasted out of DevTools, edited by hand, and will lose tomorrow.
That workflow is universal, undignified, and nobody has named it.

---

## 2. Principles

These are not aspirations; each is already enforced somewhere in the codebase.

1. **Evidence over description.** If Gabriel shows something, it happened. Where
   Gabriel infers, it says it is inferring.
2. **Prefer the highest rung.** Documentation, safe default, binding, structural
   impossibility — see the ladder in the README. Most fixes here were a move up
   it, not a new feature.
3. **A secret is referenced, never inlined.**
4. **An artifact carries its own truth.** What a consumer needs to trust a file
   is in the file, because the terminal that produced it does not travel.
5. **Nothing leaves without an explicit act.** No telemetry, no update check, no
   crash reporting. Audited, not assumed.
6. **Say what is not known.** JWT signatures are not verified; Gabriel says so
   every time rather than implying verification it cannot do.

Principle 6 is the differentiator most likely to be dismissed and most likely to
matter. Trust compounds from admitted limits, not from confident output.

---

## 3. The first five minutes

### Measured reality

From `init` to a successful authenticated replay, expert speed, no thinking
time: **3.1 seconds of machine time, six commands, six new concepts** —
collection, proxy, CA trust, capture id, promotion, session.

The machine is not the bottleneck. **Comprehension is.** Any design that
attacks the seconds is optimising the wrong variable.

### The target

Cut concepts, not seconds. Five minutes is plenty of wall-clock; the goal is to
get the user to the payoff having learned **two** concepts instead of six.

| Minute | What happens | Concepts introduced |
| --- | --- | --- |
| 0 | Install. One command. It verifies itself and says what it did. | none |
| 1 | "Show me your app." Gabriel configures the proxy, explains the CA in one sentence with the exact consequence, and waits. | trust |
| 2 | The user uses their app normally. Traffic appears live, grouped by what it looks like it is doing. | — |
| 3 | The user clicks one request. Gabriel shows: what it sent, what came back, **and what is secret in it**. | — |
| 4 | One button: *make this repeatable*. The file appears, credentials visibly absent, diff-ready. | promotion |
| 5 | Run it. It works, still authenticated. | — |

The payoff is not "you ran a request". Every tool does that. The payoff is the
moment in minute 4 when the user sees the credential **removed** and the replay
**still works**. That is the sentence they repeat to a colleague, and it is the
only thing Preview 1 needs to validate.

**Two falsifiable predictions, checkable in a week.** Users will not notice the
missing credential unprompted; and asked what they just did, they will say *"I
saved a request"* rather than *"I kept a real thing that happened so I can run
it again"*. The second is the more dangerous failure, because it looks like
success: someone who says "saved a request" will use Gabriel happily, describe
it to colleagues as a Postman alternative, and never reach what makes it
different. If either holds, the interface is the problem rather than the
engine.

---

## 4. The signature screen

One idea, not a survey of five.

### The Behaviour Timeline

Not a service map. A service map shows topology, which developers already have
from their APM. Gabriel has something no APM does: **the exact bytes, and the
ability to run them again.**

The screen is a horizontal timeline of what the application actually did, where
every element is *live evidence rather than a record*:

- **Click any request** → the real payload, secrets marked.
- **Select a span of them** → "make this repeatable" produces a runnable
  sequence, sharing session state, in the order it happened.
- **Compare two spans** → what changed between the working run and the broken
  one. This is the killer view, because "it worked yesterday" is the most common
  bug report in software and nobody can answer it.

The interaction that defines the product: **select a region of the past and turn
it into a test.** Nothing else on the market does this, because nothing else
starts from evidence.

### What it must never do

Show inferred intent *in place of* what happened — see §6.

---

## 5. The AI companion, bound to the trust model

AI in Gabriel is not a chat box bolted to a sidebar. It is one capability —
**explanation of evidence** — with the egress rules from
[information-flow.md](information-flow.md) enforced in the interface itself.

Every AI action displays, before it runs:

```
Explain this failure
  destination   local model (Ollama)          ← or a named cloud provider
  level         2 — values, secrets removed
  including     3 requests, response schemas, status codes
  excluding     Authorization, Cookie, 2 vault values
                                              [ Review payload ]  [ Run ]
```

The payload is hashed at approval and the identical bytes are sent. Approval
covers a payload, not an intention.

**Default is Level 1 and local.** Raising it is a per-action decision with the
payload on screen.

---

## 6. Three places this brief contradicts Gabriel

Recorded because they matter more than the parts that agree.

### A trust score is the thing Gabriel exists to oppose

A "Trust score: 100/100" is a single number standing in for a security property.
This codebase has spent its entire history removing exactly that pattern: a
redaction that *looked* handled while the credential sat in the open; an export
that *reported success* while containing one percent of the log; an installer
that *exited zero* having failed to write.

A score is a mask. It invites the user to stop looking, which is the failure
mode the four invariants exist to prevent.

**Instead:** show the flows themselves — what left, where it went, what was
removed and by which rule. Three lines of truth beat a number. If a user wants
reassurance, give them the ability to *check*, not a badge.

### "Gabriel analysed 12,431 requests" describes a product that violates its own rules

That sentence implies a system that has already sent thousands of real requests
to a model. Under Gabriel's own trust levels that requires explicit approval,
per action, with the payload shown.

This is not a technicality to be waived for a demo — it is the entire
differentiator. A tool that quietly uploads your traffic to explain it is
indistinguishable from every other tool, and worse than most, because it
advertised privacy.

**Instead:** the impressive number should be what was analysed **locally**.
"I compared 12,431 captured requests on this machine; 3 of them left it, and
here they are." That is a stronger claim *and* a truthful one.

### Business intent must annotate the truth, never replace it

Replacing `POST /api/v1/cart` with "Customer adds flight to shopping cart" is
the most seductive idea in the brief and the most dangerous. It is inference
presented as fact. When the guess is wrong — and it will be, on the internal,
badly-named, legacy endpoints where help is most needed — the user is debugging
a fiction.

Fidelity is Gabriel's foundational property: *if Gabriel shows it, it happened.*

**Instead:** show both, with the guess clearly subordinate and attributed.

```
POST /api/v1/cart                    ← always present, never replaced
  ~ probably: adds an item to a cart    (inferred, local model)   ✎ rename
```

The user can promote the label to a real name, which is *their* assertion rather
than the model's. Now the interface is honest and gets better with use.

---

## 7. Competitive position

| | Starts from | Replays with real session | Proves nothing leaked |
| --- | --- | --- | --- |
| Postman / Insomnia / Bruno | a request you describe | no | no |
| Charles / Proxyman | traffic you observe | no — it inspects | no |
| Playwright | a script you write | browser only | no |
| Datadog | telemetry | no | n/a |
| **Gabriel** | **traffic that happened** | **yes** | **yes, tested** |

The defensible seam is the middle column. Capture tools do not replay; replay
tools do not capture. Owning the join is the moat, and it is *engineering* work
— session semantics, cookie scoping, credential substitution — rather than
design work, which is why incumbents have not simply added it.

---

## 8. Roadmap

Gated on evidence, not dates.

**Preview 1 — does anyone care.** Five developers. The one question: does the
promotion moment land unprompted? Everything else is noise.

**v1 — the workbench.** Only if Preview 1 says the loop matters. Timeline,
request inspection, promotion by click, diff between two spans. No AI.
Prerequisite: the four gates, of which OAuth interop remains.

**v2 — explanation.** Local models first, cloud as an explicit escalation. The
egress UI in §5 ships *with* the first AI feature, not after it.

**v3 — teams.** Shared collections, review of promoted requests, org policy for
trust levels. Enterprise features follow paying teams; they do not precede them.

---

## 9. Deliberately not designed here

Named rather than faked, with what would unblock each:

- **Screen layouts and a design system.** Colour systems and type scales
  invented before a single user has been observed are decoration. Unblocked by:
  Preview 1 observations plus a designer.
- **Enterprise architecture** (RBAC, SSO, policy engine, compliance). Unblocked
  by: a team that wants to pay, whose actual constraints will differ from
  anything guessed now.
- **Financial projections.** Not something this document can honestly contain.

The most valuable thing in this file is §6, and the second most valuable is the
prediction in §3 that Preview 1 can prove wrong within a week.
