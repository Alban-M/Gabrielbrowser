# Observation sheet — Preview 1, first session

One per participant. The protocol and the reasoning behind every field are in
[preview-1.md](preview-1.md); this is the thing you hold during the session.

> **Do not show this to the participant.** It names the moments being tested,
> which is the one thing that cannot be un-told.

Fields are fixed on purpose. Five sheets with the same boxes can be compared;
five pages of prose cannot.

---

## Before they arrive

- [ ] Phone face down, notifications off, keyboard out of your reach
- [ ] Install link ready to paste: `https://github.com/Alban-M/gabriel-releases`
- [ ] Opening line open in front of you, to read rather than recall
- [ ] Clock visible — wall-clock times get written down, durations get worked out later
- [ ] A place to write questions you are **not** going to answer

**Participant** ______________  **Date** ____________  **Observer** ____________

**OS / arch** ______________  **Shell** __________  **Corporate proxy / managed machine?** Y / N

**Had they seen or heard Gabriel described before?** none / described / shown

---

## The opening line — read it, do not improvise

> Here's a link to install Gabriel. You have an API you work on — get a request
> from it into a file you could commit, and then run that file again. Think
> aloud if you can. I'm going to be quiet.

Then stop talking. If they ask you something, write it in **Questions asked**
and say only: *"I'm going to stay quiet, but I'm writing that down."*

---

## Timings — wall-clock, filled as they happen

| Moment | Time | Reached? |
| --- | --- | --- |
| Link sent | | — |
| `gabriel --version` prints | | Y / N |
| First real request appears in `capture ls` | | Y / N |
| `gabriel run` returns a 2xx | | Y / N |
| Gave up / session ended | | — |

**Install failed?** Y / N — if yes, attach `gabriel feedback` output and write the
cause here, not a guess: _______________________________________________

---

## Silences — the primary finding

Anything past roughly twenty seconds. What was on screen matters more than the
length.

| Time | On screen | What they did next |
| --- | --- | --- |
| | | |
| | | |
| | | |
| | | |

## Questions asked — written down, not answered

| Time | Question, as asked |
| --- | --- |
| | |
| | |
| | |

## "Oh, I thought it would…" — expectation breaks

Each one is a workbench design defect, not a docs gap. Their words.

1. ______________________________________________________________________
2. ______________________________________________________________________
3. ______________________________________________________________________

## Two specific moments

**The CA step.** installed without comment / read it first / asked about it /
**refused** / never reached it

Their words: _____________________________________________________________

**The promoted file has no credential in it.** Did they notice unprompted?

noticed and said so / looked at the file but said nothing / never opened the file /
had to be shown afterwards

Their words: _____________________________________________________________

## First thing they tried that Gabriel does not do

_________________________________________________________________________

---

## The end question — after the task, never during

Ask exactly this. Do not rephrase, and do not ask what *promote* means.

> "A colleague walks over and asks what you just did. What do you tell them?"

**Verbatim.** Their words, including the false starts. Paraphrasing into the
answer we wanted is how this test gets quietly failed.

```
____________________________________________________________________________

____________________________________________________________________________

____________________________________________________________________________
```

**Reading** — tick one, from what is written above, not from impression:

- [ ] **Landed** — kept a real thing they can run again and trust
- [ ] **"Saved a request"** — the artifact landed, the point did not
- [ ] **"Recorded some traffic"** — promotion was invisible
- [ ] Something else: ____________________________________________________

Cross it with the credential observation — the two together name the fix:

| | Said "saved a request" | Said "a real thing I can run again" |
| --- | --- | --- |
| **Noticed the credential was gone** | Naming problem — fix the vocabulary | Landed |
| **Never noticed** | Staging problem — fix when the moment is shown | Investigate: may be describing intent, not what they saw |

**Cell:** ______________________

---

## After the session

Only now demonstrate what they missed, and write down the gap between what they
found and what is there:

_________________________________________________________________________

**Anything you wanted to fix while watching** — goes to the workbench backlog
with this observation attached, not into the CLI:

_________________________________________________________________________

---

## Day 7 — send the five questions, nothing else

Date sent ________  Replied ________

1. Last opened it, and what for: _________________________________________
2. Used *instead*, and for what task: ____________________________________
3. One thing that would make them use it tomorrow: _______________________
4. Anything that worried them: ___________________________________________
5. Would it bother them if it disappeared: _______________________________

**Retained?** Y / N — opened it again in the week for something they chose to
do. Used once during the session and never again is **not** retained, whatever
questions 3 to 5 say.
