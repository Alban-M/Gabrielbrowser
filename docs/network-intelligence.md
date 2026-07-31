# The engineering evidence platform

The strategy for what Gabriel becomes, the category it belongs in, and the
constraint that decides how far it goes.

Network intelligence is a subcategory of this, not the ceiling. Software
produces evidence constantly — captured traffic, HARs, PCAPs, traces, logs,
failed CI runs. Almost all of it is read once and thrown away. Gabriel's purpose
is to turn that evidence into something reusable, and its differentiator is that
some of it can be **run again**.

Written the day CI first went green, before the first tag, with 11,687 lines of
shipped code operating entirely at layer 7, no packet-capture dependency of any
kind, and no user outside the machine that wrote it. Numbers that sound like
measurements were measured; everything else is a hypothesis, and §11 says which.

---

## 1. The north star

> **Evidence you can run.**

Two words carry it. **Evidence**: what happened, faithfully, not what someone
described. **Run**: not a record of it — the thing itself, again.

Every competitor has the first half. None has the second.

I want to argue against the alternative framings, because the difference is not
cosmetic. *"The memory layer for software"* and *"understand everything your
software did"* are both passive. A memory that cannot act is a log, and the
market has excellent logs. The verb is the product: Gabriel's claim is not that
it remembers better, it is that **what it remembers is executable.**

---

## 2. The category, reframed

Gabriel is not in the packet-analysis category and should stop measuring itself
against its incumbents. It is the layer above them, and they are its inputs.

```text
runnable evidence                        contextual evidence
PCAP · HAR · DevTools export ·           OTel traces · Jaeger · Datadog ·
proxy capture · cURL · OpenAPI           k8s events · cloud audit · CI logs
        │                                          │
        │  contains the bytes                      │  describes what happened
        ▼                                          ▼
    ┌───────────────────────────────────────────────────┐
    │                    evidence                        │
    └───────────────────────────────────────────────────┘
              │                              │
          re-run                        understand · correlate
              │                              │
              └──────────────┬───────────────┘
                             ▼
     tests · OpenAPI · load scripts · docs · incident reports
```

**These two kinds must not be conflated, and the expanded source list is where
that mistake gets made.** A PCAP and a HAR contain the actual bytes: they can be
replayed. An OpenTelemetry span, a Jaeger trace, a Datadog export and a
Kubernetes event are *descriptions* — sampled, body-less, often lossy. There is
nothing in them to send.

Ingesting them is worth doing, for correlation: *this trace points at that
captured request; here is the request, and here it is running again.* But a
product that treats traces as first-class evidence and cannot run them has
quietly become an observability tool — understanding without execution, which is
the passive category the north star exists to reject. The market already has
excellent observability.

So: **contextual evidence enriches runnable evidence. It never substitutes for
it, and the interface must never imply that it can.**

Wireshark, Charles, DevTools and the rest become **evidence sources**, not
rivals. That is a strictly better position than "a nicer Wireshark", because it
turns twenty years of accumulated dissectors from a moat protecting an incumbent
into a supply chain feeding Gabriel. Their output becomes the raw material for
the one operation none of them perform.

It also resolves the strategic bind cleanly: there is no reason to fight for the
packet-inspection user, because their tool now produces Gabriel's input.

---

## 3. Every artifact becomes executable

The platform statement, and the thing worth building toward:

| From | Gabriel produces | Status |
| --- | --- | --- |
| A captured request | a committable file that replays, credentials removed | **shipped** |
| A span of captured traffic | an ordered suite sharing session state | designed |
| A HAR from a colleague | the same, from their machine's evidence | **shipped** |
| A PCAP from the network team | the HTTP conversation inside it, runnable | near |
| A captured flow | OpenAPI, k6, Playwright, JMeter | adjacent |
| A failed run | an incident report with the evidence attached | adjacent |
| A production trace | a regression test in CI | future |

One row of that table exists today, and it is the row the others are built on.
The direction is right; the sequencing in §10 is what makes it survivable.

---

## 4. The constraint nobody has named: replay is bounded by idempotence

This is the part I want to add to the brief, because it decides how far "across
the stack" can actually go, and getting it wrong would break the core promise
rather than merely slow it down.

**Replaying an HTTP GET is safe. Replaying a payment POST charges the card
twice.**

Gabriel is currently safe by accident: developers replay reads while debugging,
and a duplicated `GET /users/me` costs nothing. That accident does not survive
contact with the brief's protocol list.

| Protocol | Replaying it means | Danger |
| --- | --- | --- |
| HTTP GET | fetch again | none |
| HTTP POST/PATCH/DELETE | **perform the action again** | duplicate orders, charges, deletions |
| gRPC unary | depends entirely on the method | invisible from the wire |
| **Kafka produce** | **re-publish to a live topic** | downstream systems act on it |
| **Database write** | **re-execute the mutation** | corruption |
| **MQTT publish** | **re-actuate a device** | physical consequences |
| SSH | re-run a command | anything |

The brief's "anything that communicates" collapses the distinction between
**observing** a protocol and **replaying** it. Gabriel can safely *understand*
all of them. It cannot safely *re-run* all of them, and the value proposition is
the second verb.

So replay needs a safety model before it needs more protocols:

- **Classify every captured operation** as read, idempotent write, or
  non-idempotent — from method and protocol semantics where they are
  knowable, and marked unknown where they are not.
- **Non-idempotent replays require confirmation**, showing what will be
  performed — the same build → show → approve → execute discipline as the AI
  payload and the installer checksum, applied to actions rather than data.
- **A dry-run mode** that resolves everything and sends nothing is the correct
  default for anything unclassified.
- **Never** extend replay to a protocol whose operations cannot be classified.
  Understanding it is still valuable; running it is not on offer.

This is not a limitation to be apologised for. It is the difference between a
tool a developer trusts against staging and one they will run against
production — and the second is where the money is.

---

## 5. Memory, and what makes it worth having

Rejecting *"the memory layer for software"* as a headline does not mean
rejecting memory. It means the headline needs the verb. As a capability, memory
is real and underexploited:

> Gabriel keeps the behaviours somebody thought were worth keeping.

The word doing the work is **kept**, not *recorded*. A tool that stores
everything its proxy ever saw is a landfill with a search box; the value comes
from promotion being a deliberate act. A promoted request is a behaviour a human
decided mattered, in a form that still runs.

That yields something no log or trace store can offer: **the same behaviour, run
at two points in time, compared.** "This endpoint's contract changed on the
14th" is answerable from evidence rather than inferred from dashboards, and
`gabriel diff` is the smallest version of it that already exists.

## 6. Engineering intelligence, not "AI features"

The right framing, and the shape it takes:

```text
traffic → evidence → understanding → replay → automation → verification
```

Most of what gets called AI here is not AI. Retransmissions, duplicate ACKs,
MTU discovery, certificate expiry and DNS failures are **deterministic
analyses**. Implementing them as prompts makes them slower, more expensive,
non-reproducible, and occasionally wrong. Ship them as code; let a model explain
the result to whoever is not a network engineer.

The genuinely strong cell is **artifact generation** — capture in, OpenAPI or
k6 or Playwright out. It needs no values (Level 1 under
[information-flow.md](information-flow.md)), the output is checkable by running
it, and it is a direct extension of promote rather than a new capability bolted
alongside.

Everything here stays inside the four invariants: declared trust level, payload
bound by hash at approval, an egress record written before the send, and a
reproducible decision.

---

## 7. Behaviour, not packets

Agreed, and already the design direction — the signature screen in
[design.md](design.md) is a behaviour timeline, not a packet list.

```text
not:  Packet 5832  ACK  win=64240  seq=1029384
but:  Checkout → inventory → pricing → tax → payment ✗ → email skipped
```

The packets remain underneath and are reachable in one click. What changes is
the default: the summary before the detail, and the detail available rather than
imposed.

The addition worth making explicit: **the timeline must show what it inferred
separately from what it observed.** "Checkout" is a guess about a sequence of
requests; the requests are facts. A timeline that presents the guess as a fact
is the fidelity failure this product cannot afford, and the rule for it is
already written in [vision.md](vision.md) §6.

---

## 8. Three technical realities

These do not change with the framing, and they constrain any packet-layer work.

**TLS does not become visible by moving down a layer.** Below the boundary is
ciphertext. Reading bodies needs session keys or a MITM proxy — which Gabriel
already has. For the HTTPS traffic that is most of what users care about,
packet capture would show *less* than the L7 proxy shows today.

**Promiscuous capture inverts the trust model.** Every invariant rests on
Gabriel seeing only traffic the user routed through it. An interface capture
holds credentials for services they never chose to inspect and cannot
enumerate. That is a second product with its own consent question, not a flag
on this one.

**Privileges break the install story.** libpcap, Npcap, WinDivert and eBPF mean
drivers, signing, notarisation and asking for root — against an installer that
today needs none, and on the exact first five minutes the preview exists to
measure.

Which is why **PCAP import, not live capture, is the move**: no driver, no
privileges, no new consent model, and it turns *"here is the capture the network
team sent you"* into something runnable. That sentence is not available to
Wireshark, and it costs a parser rather than a company.

---

## 9. Users

The three personas that share Gabriel's actual job — *reproduce what happened*:

| Persona | The moment | Why nothing else serves it |
| --- | --- | --- |
| **Backend / API developer** | "It worked yesterday" | Nobody kept yesterday's bytes runnable |
| **QA engineer** | Turning a real flow into a regression test | Hand-written tests encode assumptions, not behaviour |
| **Support engineer** | Reproducing a customer's failure | A customer's HAR is inert; this one runs |

The third is the most undervalued in the original brief. Their core task *is*
Gabriel's core operation, no incumbent does it, and they sit inside companies
with budget.

The other twenty personas are real people whose problems mostly belong to other
tools. Serving them all first is how a product becomes a worse version of six
things.

---

## 10. Roadmap, gated on evidence

**Now.** Tag the preview. Five developers. One question: does the promotion
moment land without being explained? Nothing below starts before that answers,
because if the answer is no, none of it helps.

**If the loop lands.** The replay safety model from §4 — before more protocols,
not after. Then gRPC and GraphQL replay, OpenAPI generation, HTTP/2, PCAP
import. Each reinforces *evidence you can run*.

**If teams appear.** Shared collections, org policy on trust levels, egress
reporting — the last two are by-products of invariants 3 and 4 rather than new
work.

**If organisations appear.** The scale is individual → team → organisation, and
the thing that changes at the last step is *time*: a body of verified behaviours
accumulated across services and years. The valuable operation there is not a
knowledge graph — that is where products go to become unfalsifiable — it is
**behaviour compared across time**: which contracts changed, when, and what
broke as a result. Concrete, checkable, and a direct extension of `diff` rather
than a new discipline.

**If a specific customer needs it.** The packet layer, as a separate privileged
process with its own consent boundary, justified by that customer rather than by
the category.

---

## 11. What this does not contain

Named rather than faked: screen-by-screen specifications, a component library,
wireframes for features that do not exist, three-year financials, a plugin SDK,
and compliance roadmaps. Each is unblocked by the same thing — somebody using
the product — and each would be invention today.

---

## 12. Where this goes

Gabriel should not aspire to be a better Wireshark. It should become the
workspace where software behaviour is understood, replayed, verified, explained
and turned into engineering assets — a place where a PCAP, a HAR, a DevTools
export and a production trace are all just evidence, and every one of them can
be run again.

In that future a PCAP, a HAR, a DevTools export and a production trace are all
just evidence, and Wireshark is one of the things that produces it.

The first step is not a smaller version of that ambition; it is the same claim
at the smallest scale that can be tested. A developer captures one request,
promotes it, and watches a file with no credential in it come back
authenticated. If that lands, everything in this document is a sequencing
question. If it does not, none of it was the problem — which is why the next
action is five people and a tag.

Every engineering system leaves evidence, and almost all of it is read once and
discarded. Gabriel's purpose is to make that evidence trustworthy,
understandable and reusable — so that evidence stops being where an
investigation ends and becomes where the engineering starts.

**Evidence you can run.**
