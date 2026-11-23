# Research a question

**Purpose:** a finished run has answered the question in the chat, with the
load-bearing claims resting on sources the run read, and what couldn't be
verified said plainly.

**Before you start:** the workspace needs your computer (`search`, `read`),
and the web needs `web_search` and `web_fetch`. Use what this run has, and
say in the answer what you couldn't reach.

## Inputs

- The question, precisely. If there's any doubt about what was asked,
  restate it in the answer's first line.
- What would settle it: a document, a figure, an official statement.

## Steps

1. **Search the workspace first,** briefly: if the owner's own material
   answers it, answer from there and say so.
2. **Otherwise search the web,** with two or three phrasings, and read the
   strongest results in full with `web_fetch`: the primary source over the
   summary of it, the official page over the article about it. If a site
   isn't granted, ask through the usual request and go on with what you
   have.
3. **Cross-check** every load-bearing claim against a second source where
   one exists. Note disagreements as they're found, not at the end.
4. **Answer in the chat:** the answer first, then the grounds, each
   load-bearing claim with the source it rests on, named so the owner can
   find it. Give dates for anything time-sensitive: when the source was
   published or updated, not when it was read.
5. **End with what could not be verified,** in those words, listing each
   item: a claim found in only one weak source, a figure whose primary
   source was unreachable, a point the sources disagreed on without
   resolution.

## Guardrails

- A search snippet is never the source for a claim in the answer.
- Never average two disagreeing figures into a third.
- Never fill a gap with what's probably true: a labelled gap is a correct
  answer, an unlabelled guess is not.
- Don't go around a paywall, a login or a refusal: use what's granted, and
  say what was out of reach.

## Stop and ask

- The question turns out to be two questions with different answers.
- The only sources are ones the owner wouldn't trust, and better ones are
  out of reach.
- Answering needs a grant the run doesn't hold: ask for it through the
  usual request, rather than working around it.
