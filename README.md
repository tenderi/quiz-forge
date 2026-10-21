# quiz-forge

A self-hosted question bank for quiz-night hosts, driven by a Telegram group and a
small web app.

- Members **submit** questions with rich metadata: difficulty, theme, kind (text,
  multiple choice, multi-part, image, audio, tiebreaker), background story, fun
  fact, sources and more.
- They're **warned about near-duplicates** at submission. The warning never blocks.
- The group **votes** a question in, and **iterates** on it through edit
  proposals, so one agreed current version exists, with full history.
- An optional **AI review pass** fixes typos, proposes fun facts and fact-checks.
- Hosts **assemble quiz nights** from templates (rounds × slots with constraints),
  filled deterministically from a seed, then **print** host sheets, answer sheets
  and picture handouts.
- House rules: no yes/no questions, and numeric answers only in tiebreakers
  (years excepted). Expect a "Boo!".

Status: **design phase**. See [`docs/DESIGN.md`](docs/DESIGN.md).

One deployment serves one group. The code is public; your question bank lives only
in your own database and is never part of this repository.

## License

MIT
