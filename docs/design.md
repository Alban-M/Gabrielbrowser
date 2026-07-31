# Design

The parts of the product experience that prose can carry. The signature screen
is not described here because it is built —
[`docs/mockup/workbench.html`](mockup/workbench.html). Strategy and positioning
live in [vision.md](vision.md); this is the layer beneath it.

Everything here is a hypothesis until [Preview 1](preview-1.md) contradicts it.

---

## 1. Information architecture

Three groups, in the order a user's trust moves through them. Not eight flat
sections — the grouping *is* the argument.

```
EVIDENCE        what happened            Timeline · Requests · Sessions
ASSETS          what you made from it    Tests · Suites · Reports
BOUNDARY        what left, and why       Egress · Policies
```

**Why this and not Capture / Replay / Tests / Security.** A section called
"Security" is a place users visit once and never again. Splitting it into
*Boundary* — a permanent count of what has left the machine — makes the claim
ambient instead of ceremonial. The nav shows `0` beside Egress at all times.
That zero is the product.

Evidence is read-only by construction. Assets are the only writable things.
That separation means "can I break my captures by editing?" has a structural
answer rather than a documented one.

---

## 2. Journeys

### The developer, debugging (primary)

> "It worked yesterday."

Open Timeline → find yesterday's run and today's → select the same span in each
→ **Compare**. The diff is over *evidence*, not logs: same request, different
response. Today's tools cannot answer this because they never had yesterday's
bytes.

Exit: a promoted regression test that fails, which is a bug report a colleague
can run.

### The QA engineer, building a suite

Filter the timeline to a session → select a span → **Make repeatable** →
Gabriel writes an ordered suite sharing session state. The assertion is
generated from what the response *actually was*, and shown for editing before
it is saved — a generated assertion nobody reviewed is a test that encodes a
bug as a requirement.

### The security engineer, auditing

Boundary → Egress. Expected reading: empty. The interesting screen is the one
that stays empty, which is why it carries a count rather than a chart.

### The engineering leader

Not a persona this product serves yet, and pretending otherwise designs a
dashboard nobody opens. Revisit when teams share collections.

---

## 3. Design system

### Colour

| Token | Light | Dark | Role |
| --- | --- | --- | --- |
| `--ground` | `#F2F5F6` | `#0C1416` | page |
| `--surface` | `#FFFFFF` | `#121D20` | panels |
| `--ink` | `#101A1E` | `#E2ECEE` | primary text |
| `--accent` | `#0F6E7A` | `#4FAAB6` | interaction, one hue only |
| `--kept` | `#2E6B45` | `#6FBF8E` | retained / verified |
| `--removed` | `#9C3E2C` | `#DB8E7C` | redacted / failed |
| `--warn` | `#8A5D08` | `#D6A840` | needs attention |

Neutrals are biased cool toward the accent rather than being pure grey — a
default grey reads as unconsidered. Semantic colours are deliberately *not* the
accent: when everything is teal, nothing is.

The accent is a deep instrument teal, not a neon cyan. Neon on near-black is
the security-tool cliché and it signals threat. Gabriel's brand problem is the
opposite: **security confidence without security anxiety.** A calm instrument,
not an alarm.

### Type

Monospace carries *structure* — headings, labels, data, navigation counts.
Sans carries *prose* — explanations and help.

That inversion is the identity. Gabriel's subject matter is literally
monospace: headers, paths, tokens, hashes. Setting the interface in the same
voice as the evidence says the tool and its material are made of one thing.
Sans appears only where a human is being spoken to.

`font-variant-numeric: tabular-nums` everywhere digits stack. Timestamps that
jitter make a timeline unreadable.

### States

- **Empty** states name the next action, never apologise: *"No captures yet.
  Start the proxy and use your app — traffic appears here live."*
- **Error** states say what happened, what it means, and the fix — the shape
  `gabriel doctor` already uses.
- **Loading**: nothing spins under 300 ms. The measured cold start is 7.5 ms;
  a spinner would be inventing latency to look busy.
- **Redaction** is drawn as a hatched strike-through, never a solid block. A
  black box says "hidden"; hatching says "removed, and here is where it was".

### Motion

One rule: motion tracks state that actually changed. The recording dot pulses
because recording is genuinely ongoing. Nothing else animates. All of it inside
`prefers-reduced-motion`.

---

## 4. Command palette

`⌘K`. Verbs first, because a user knows what they want before they know where
it lives.

```
> compare this span with yesterday
> make repeatable                        ⌘⇧T
> explain why this failed                     local model
> export everything as HAR                    complete by default
> export the newest 1000 as HAR               declares itself partial
> show what has left this machine        ⌘⇧E
```

Two entries for export, deliberately. The safe one is not hidden behind a flag
on the dangerous one — both are visible, and the partial one carries its
consequence in its own label. Hiding the truncating variant would make it feel
advanced rather than lossy.

Every destructive or egressing command shows its consequence in the palette
row, before selection.

---

## 5. The AI experience

The interaction is the approval, not the answer. Before anything runs:

```
Explain why this failed
  destination   local · Ollama llama3            [ change ]
  level         2 — values, secrets removed
  including     3 requests · response schemas · status codes
  excluding     Authorization, Cookie, 2 vault values
  payload       4.1 KB                      [ Review exact bytes ]
                                         [ Cancel ]  [ Run locally ]
```

Bound to [information-flow.md](information-flow.md): the payload is hashed at
approval and the identical bytes are sent. Default Level 1, default local.

**The model proposes; it never acts.** A suggested request appears as a draft
the user runs, never as one that ran. Model output is server-controlled text
and gets the same escape-defusing as any response body.

**No chat box.** A chat window invites unbounded context, which is the exact
pressure the trust levels exist to resist. Every AI action is a named verb with
a declared level — which is also what makes it reviewable.

---

## 6. Enterprise

Ordered by what a team hits first, not by what sells first.

1. **Shared collections** — promoted requests are already files. Review happens
   in the pull request that already exists. This needs almost nothing built.
2. **Org policy on trust levels** — an organisation caps the maximum level;
   Level 3+ can be disabled centrally. Policy belongs where the boundary is.
3. **Egress reporting** — per-workspace, from the same records invariant 3
   requires. Compliance evidence as a by-product rather than a feature.
4. **SSO / RBAC** — last, because they gate nothing until there are seats.

Enterprise complexity stays out of the developer's way by living in
**Boundary**, which is one nav group they can ignore until an admin sets a
policy that affects them — at which point the affected action says so inline.

---

## 7. The landing page

Hero, and it is a claim rather than a category:

> ### Your app already told the truth.
> ### Gabriel writes it down.
>
> Record what your application actually did. Turn any of it into a file with
> the credentials removed — that replays, still authenticated.

Then the one thing worth showing above the fold: **the promotion proof table**
from the mockup. Captured on the left, committed file on the right, the rule
that made each change. No screenshot of a request builder; everyone has seen a
request builder.

Below: the three-line install, the four invariants stated plainly, and the
known limits — including that JWT signatures are not verified. Publishing the
limits on the landing page is not modesty; it is the differentiator. Every
competitor's page claims completeness.

No pricing until someone has asked to pay.

---

## 8. What is still unknown

The honest list, because a design document that projects certainty is the same
failure as a trust score:

- Whether the timeline or the request list is the screen people live in.
- Whether "make repeatable" is understood without being explained. **The
  prediction on record is that it is not** — see [vision.md](vision.md) §3.
- Whether the egress ledger reads as reassuring or as unnerving. It could
  plausibly make users think about a risk they had not considered.
- Whether monospace-first reads as precise or as austere.

Each is answerable in one week of watching five people, and not answerable at
all by more design.
