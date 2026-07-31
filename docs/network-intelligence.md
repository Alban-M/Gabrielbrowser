# Network intelligence: an assessment

A response to the proposal that Gabriel become a network intelligence platform
replacing Wireshark, Charles, DevTools and Postman.

Written the day CI first went green and before the first tag. Gabriel is 11,687
lines of shipped code, operates entirely at layer 7, has no packet-capture
dependency of any kind, and has been used by nobody. Every number below that
sounds like a projection is a hypothesis; the ones that sound like measurements
were measured.

The short version: **most of the brief is achievable, one part of it is a
different company, and the strongest idea in it is not the one it leads with.**

---

## 1. The premise, examined

The brief's target is "developers say I never want to open Wireshark again."

That framing loses, and it is worth being precise about why rather than
enthusiastic about the goal.

Wireshark is not weak where the brief attacks it. Its users are not underserved
by visualisation — they are served by **thousands of protocol dissectors
accumulated over two decades**, and by being the tool whose output a network
vendor will accept in a support ticket. A prettier packet list competes with the
one thing Wireshark is genuinely excellent at, using a fraction of the
investment, against an incumbent that is free.

Meanwhile the brief's own list contains the thing Wireshark genuinely cannot do,
mentioned once and not developed:

> **Every request should be replayable.**

Nothing in the packet-analysis category can re-run what it observed. Wireshark
shows you what happened. Charles shows you what happened, prettier. DevTools
shows you what happened, in a browser. **Gabriel already re-runs it, with the
session intact and the credentials removed.**

That is the seam. It is not "understanding instead of packets" — understanding
is a feature anyone can add and several competitors are adding. It is that
observation and execution are the same object.

**Recommendation: do not go *down* the stack for visibility. Go *across* the
stack for replayability.** The defensible sentence is not "we explain your
network", it is:

> Anything Gabriel can see, Gabriel can run again — safely, and in CI.

---

## 2. Three technical realities the brief passes over

### TLS does not become visible because you moved down a layer

Capturing packets does not let you read HTTPS. Below the TLS boundary you get
handshake metadata, SNI, certificate chains, timing and sizes — and ciphertext.
To read bodies you need either the session keys (`SSLKEYLOGFILE`, which the
application must cooperate to produce) or a man-in-the-middle proxy with a
trusted CA.

Gabriel already has the second one. So for the HTTPS traffic that is the
overwhelming majority of what its users care about, **packet capture would add
less visibility than the L7 proxy already provides, not more.**

Where packet capture genuinely adds something: non-HTTP protocols, transport
pathology (retransmissions, MTU, RST), DNS, and traffic from applications that
cannot be pointed at a proxy. Those are real, and they are a different job.

### Promiscuous capture inverts the trust model

Gabriel's entire security posture assumes it sees **only traffic the user
deliberately routed through it**. The four invariants in
[information-flow.md](information-flow.md), the allow-list feedback bundle, the
"nothing has left this machine" ledger — all of it rests on that.

A packet capture engine sees everything on the interface: the user's password
manager syncing, their mail client, a colleague's traffic on a shared segment.
Gabriel would begin holding credentials for services the user never chose to
inspect and cannot enumerate.

That is not a reason to refuse it. It is a reason to treat it as a **second
product with its own consent model**, not a feature flag on this one. The
consent question — "may this tool record everything this machine does" — is
categorically different from "may this tool proxy the app I am debugging", and
answering it wrongly would destroy the one asset Gabriel has that competitors
do not: a defensible claim about what it does not do.

### Elevated privileges change the install story

`libpcap`, `Npcap`, `WinDivert` and eBPF all require administrator rights, a
kernel extension, or a signed driver. Today Gabriel installs with
`curl … | sh` into `~/.local/bin` and needs no privileges at all — which is
precisely why the installer has a regression suite and a smoke gate.

Adding a capture driver means code signing, notarisation, kernel-extension
review on macOS, driver signing on Windows, and an install flow that asks for
root. That is months of work in which no user-visible feature appears, and it
raises the bar for the very first five minutes that the whole preview is
designed to measure.

---

## 3. Users: 23 personas is not a market

The brief lists 23. They do not share a job, and a product serving all of them
serves none of them first.

The three that share **Gabriel's actual job** — *I need to reproduce what
happened*:

| Persona | The moment they need Gabriel | Why nothing else does it |
| --- | --- | --- |
| **Backend / API developer** | "It worked yesterday" | Nobody kept yesterday's bytes in a runnable form |
| **QA / test engineer** | Turning a real user flow into a regression test | Hand-written tests encode assumptions, not behaviour |
| **Support / solutions engineer** | Reproducing a customer's failure locally | A HAR from the customer is inert; this one runs |

The rest are real people with real problems that mostly belong to other tools.
An SRE with a packet loss question is served by their APM and by Wireshark. A
SOC analyst is served by Zeek and Splunk. Chasing them is how a product becomes
a worse version of six tools.

**One persona in the brief is undervalued: support engineers.** They are the
only group whose core task — *reproduce the customer's problem* — is exactly
what Gabriel does and no incumbent does. They also sit inside companies with
budget.

---

## 4. Scope, tiered by distance from what exists

| Capability | Distance | Assessment |
| --- | --- | --- |
| gRPC, GraphQL, SOAP capture + replay | **Adjacent** | All L7 over HTTP/2. Reuses the engine. Weeks. |
| HTTP/2 and HTTP/3 in the proxy | **Adjacent** | Real work, known shape, no new trust model |
| MQTT, AMQP replay | **Near** | New codecs, same architecture, same consent model |
| OpenAPI generation from captures | **Adjacent** | High value, purely additive, no new risk |
| Live dashboards over captured L7 | **Near** | Data already exists; presentation work |
| PCAP / PCAPNG **import** | **Near** | Read-only, no driver, no privileges. Cheap and strategically interesting |
| Live packet capture | **Different product** | Drivers, privileges, new consent model, new invariants |
| eBPF, Kubernetes/service-mesh capture | **Different product** | Plus a cluster-side agent, i.e. a deployment story |
| NetFlow, VPC flow logs, Zeek, CloudTrail | **Different product** | This is an observability company |
| Plugin marketplace | **Premature** | Marketplaces need users on both sides; there are zero on one |
| SOC2 / ISO27001 / FedRAMP | **Premature** | These are answers to a customer's procurement, and there is no customer |
| Mobile companion | **Premature** | No validated desktop workflow to companion |

**PCAP import is the sleeper.** It gets Gabriel into the packet conversation
with no driver, no privileges, and no new trust model — the file is already on
disk and the user chose to hand it over. It is also the natural bridge: *"here
is the capture your network team sent you; here is the HTTP conversation inside
it; now run it again."* That sentence is not available to Wireshark, and it
costs a parser rather than a company.

---

## 5. If the packet layer is pursued anyway

It should be a **separate process with a separate consent boundary**, not a
module.

```text
gabriel-capture (privileged, minimal, auditable)
        │  a narrow local socket; only summaries cross it
        ▼
gabriel (unprivileged, everything else)
```

Properties that follow, all of which are consequences of the existing
invariants rather than new policy:

- The privileged component is as small as it can be and does **one** thing.
- What crosses the boundary is declared, the way an AI payload is declared —
  and for the same reason.
- The capture scope is an allow-list of interfaces and hosts, never "everything".
- The egress ledger gains a second row it currently cannot have: *what this
  machine recorded*, distinct from *what left it*.

If that separation is not worth building, the packet layer is not worth
building, because the shortcut is the thing that would sink the product's one
genuine differentiator.

---

## 6. AI, bounded by what already exists

The brief lists ~30 AI features. They collapse into four shapes, and the
constraint is already written down in [information-flow.md](information-flow.md):
trust levels, payload binding, an egress record, and a reproducible decision.

| Shape | Example | Level | Note |
| --- | --- | --- | --- |
| Explain observed structure | "Explain this TLS failure" | 1 | Handshake metadata is not payload |
| Explain observed content | "Why did checkout fail" | 2–3 | Level 2 is "no known secret", not "nothing sensitive" |
| Generate an artifact | OpenAPI, k6, Playwright | 1 | Shape in, code out; the highest-value cell in this table |
| Diagnose transport | "Find retransmissions" | 0–1 | Statistics, not content — mostly not an AI problem at all |

Two things worth saying plainly, because the brief's framing invites the
opposite:

**Most of the "AI" list is not AI.** Finding retransmissions, duplicate ACKs,
MTU problems and certificate expiry are deterministic analyses. Implementing
them as prompts makes them slower, more expensive, non-reproducible and wrong
sometimes. Ship them as code and let the model explain the result.

**Artifact generation is the strongest cell.** "Turn this captured flow into a
k6 script / an OpenAPI document / a Playwright test" is Level 1, needs no
values, produces something the user can check by running it, and is a direct
extension of the promote step rather than a new capability bolted beside it.

---

## 7. Monetisation: what can honestly be said

Nothing about pricing can be derived from a product with no users. What *can* be
said is which shapes are structurally available:

- **Free, local, unlimited** is not a tier to be minimised — it is the proof of
  the privacy claim, and taking it away later would cost more than it earns.
- **Team** is the first honest paid unit, because sharing promoted collections
  is where a second person appears.
- **Enterprise** is bought for policy and audit — the trust-level ceiling and
  egress reporting from invariants 3 and 4, which is a by-product rather than a
  feature to be built.
- **AI credits** only if inference is hosted; local-first weakens that line and
  strengthens the product. Prefer the product.

Anything more specific would be invention.

---

## 8. Competitive position, honestly

| | Sees packets | Reads HTTPS bodies | **Re-runs it** | Removes credentials |
| --- | --- | --- | --- | --- |
| Wireshark | ✅ | only with keys | ❌ | ❌ |
| Charles / Proxyman | ❌ | ✅ | partial, manual | ❌ |
| DevTools | ❌ | ✅ | copy as cURL | ❌ |
| Postman / Bruno | ❌ | n/a | ✅ from what you typed | ❌ |
| Burp | ❌ | ✅ | ✅ for attack | ❌ |
| **Gabriel** | ❌ | ✅ | **✅ from what happened** | **✅** |

The empty column is the fourth one, and it stays empty in the brief's expanded
version too. Adding column one puts Gabriel in a fight it cannot win; deepening
columns three and four is a fight nobody else is having.

---

## 9. Roadmap, gated on evidence rather than dates

**Now — Preview 1.** Five developers. One question: does the promotion moment
land unprompted? Nothing in this document should start before that answers.

**If the loop lands.** gRPC and GraphQL replay, OpenAPI generation, HTTP/2 in
the proxy, PCAP import. All adjacent, all reinforcing "anything Gabriel sees, it
can run again."

**If teams appear.** Shared collections, org policy on trust levels, egress
reporting. Enterprise features follow paying teams; they do not precede them.

**If a specific customer needs it.** The packet layer, as a separate privileged
component, justified by that customer rather than by the category.

---

## 10. What this document does not contain

Named rather than faked: screen-by-screen specifications, a component library,
wireframes for features that do not exist, three-year financials, a plugin SDK,
and compliance roadmaps. Each is unblocked by the same thing — **someone using
the product** — and each would be invention today.

The design work that *is* grounded is in [design.md](design.md), and it is
explicitly a hypothesis with a falsifiable prediction attached.

---

## 11. The recommendation

Do not build this yet. Not because it is wrong — several parts of it are right
and the tiered table says which — but because **the sequencing is inverted**.

Gabriel is one command from its first release, and no human outside this machine
has used it. The brief proposes a horizontal platform across 40 protocols, 23
personas and 6 clouds at the exact moment the vertical thesis is unvalidated.
If the promote → replay loop does not land with five developers, none of the
capabilities in this brief will save it; if it does, this document tells you
which ones to build first and in what order.

The one sentence worth carrying forward is not from the brief. It is the
inversion of it:

> Everything else shows you what happened. Gabriel runs it again.
