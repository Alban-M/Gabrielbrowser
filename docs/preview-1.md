# Preview 1

The goal of Preview 1 is **not to find more bugs.** The CLI is frozen; bugs
found here get fixed, but finding them is not what this is for.

It exists to answer four questions, and the answers should shape the desktop
workbench far more than any further CLI work would:

1. Do developers understand capture → promote → replay **without being told**?
2. Is the value proposition obvious, or does it need explaining?
3. Which tasks send them back to Postman, Bruno, or curl?
4. What stops someone using Gabriel as their primary API workbench for a day?

There is no telemetry and there will not be any. Everything below is measured
by watching and asking, which for five to twenty people is both feasible and
richer than counters would be.

## The first session (45 minutes, observed)

The single most valuable half hour of the whole preview, and it only works
once per person — after that they know how it works and can never un-know it.

**Rules for whoever is running it.** Say nothing beyond the opening line. Do not
answer questions during the task; write them down instead — a question asked is
a finding, and answering it destroys the finding. Do not touch the keyboard.

Opening line, verbatim, and nothing more:

> Here's a link to install Gabriel. You have an API you work on — get a request
> from it into a file you could commit, and then run that file again. Think
> aloud if you can. I'm going to be quiet.

### What to write down

Timings, because these are the metrics that would otherwise need telemetry:

| Measure | Start | Stop |
| --- | --- | --- |
| Install success | link sent | `gabriel --version` prints |
| Time to first capture | install finished | a real request appears in `capture ls` |
| Time to first replay | first capture | `gabriel run` returns a 2xx |
| Install failures | — | every one, with `gabriel feedback` attached |

Behaviour, which matters more than the timings:

- **Where they stopped.** Silence longer than about twenty seconds means the
  next step was not discoverable. Note what was on screen.
- **What they expected to happen instead.** Every "oh, I thought it would…" is
  a design defect in the workbench, not a documentation gap.
- **Whether they trusted the CA step.** This is the point where a security-minded
  developer may refuse, and refusing is a legitimate outcome worth understanding.
- **Whether they noticed the promoted file has no credentials in it.** This is
  the whole product thesis. If it has to be pointed out, the thesis is not
  landing on its own.
- **The first thing they tried that Gabriel does not do.**

Do not demonstrate anything until the task ends or they give up. When it ends,
then show what they missed and note the gap between the two.

## Day 7

Send this and nothing else. No reminders about features they did not use.

> You've had Gabriel for a week. Five questions, short answers are fine —
> and "I didn't use it" is the most useful answer you can give me.
>
> 1. When did you last open it, and what for?
> 2. What did you use *instead* of Gabriel this week, and for what task?
> 3. What is the one thing that would make you use it tomorrow?
> 4. Did anything about it worry you? Security, trust, "where did my token go".
> 5. Would it bother you if it disappeared tomorrow? Honestly.

Question 2 answers "which tasks send them back to Postman" better than asking
about Postman directly, because it does not put the word in their mouth.
Question 5 is the retention signal — a stated *would you keep using it* is close
to worthless, while a genuine reaction to losing it is not.

### Retention, without telemetry

7-day retention is: **of the people who completed a first replay, how many
opened Gabriel again in the following week for something they chose to do.**
Question 1 measures it. Someone who used it once during the observed session and
never again is not retained, whatever they say in questions 3 to 5.

## Who to invite

Five people, chosen for difference rather than enthusiasm:

- At least two who have **never** been shown Gabriel or heard it described.
  They are the only people who can answer question 1 honestly.
- At least one who works behind a corporate proxy or on a locked-down machine.
  That is where the installer and `gabriel doctor` earn their keep, and where
  the CA step is most likely to be refused outright.
- At least one on Windows, which has had the least real use.
- Ideally one who is hostile to the premise — someone happy with Postman.

## What is in scope to fix during the preview

The CLI is frozen. That means:

**Fix:** CI failures, OAuth interoperability defects found by
[the runbook](oauth-interop.md), security defects, installer and release
defects, and anything that stops a preview user getting to a first replay.

**Do not fix:** missing features, ergonomics, output formatting, anything that
begins "while I was in there". Write it down for the workbench instead. The
temptation during a preview is to close the gap the user just hit, and every
time that happens the preview stops measuring the thing it was built to
measure.

A request that arrives more than twice from different people is not a feature
request — it is a finding about the workbench, and it belongs in the workbench
backlog with the observation that produced it attached.

## Exit criteria

Preview 1 is finished when:

- five people have completed the observed first session,
- day-7 answers are in from all of them,
- every install failure has a cause,
- the OAuth interop table is filled in, and
- the four questions at the top have answers written down.

At that point the useful information is no longer in the codebase, and the
workbench design starts from what is in this document.
