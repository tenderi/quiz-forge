# quiz-forge

A self-hosted question bank for quiz-night hosts, run from a Telegram group and
a small web app.

- **Submit** questions with rich metadata: difficulty, theme, kind (text,
  multiple choice, multi-part, image, audio, tiebreaker), background story, fun
  fact, host notes, sources and tags. Use the bot's `/add` wizard or the web form.
- **Duplicate warnings** at submission (they never block).
- **Voting**: a question is in as soon as another member approves it (and the
  ✅ outnumber the ❌); it's out if the voting window runs out first.
- **One agreed version**: anyone proposes changes, the author accepts or
  declines, and the full history and diffs are kept.
- **House rules**: no yes/no questions (the group gets a shaming message), and
  number answers only in tiebreakers. Years are allowed, but every number earns
  a “Boo!”.
- **Optional AI review**: fixes typos (behind a deterministic guard), proposes
  fun facts and corrections, fact-checks with web search, spots duplicates.
- **Quiz nights**: templates of rounds and slots (“round 3 starts with three
  picture questions in one theme”), filled deterministically from a seed, with
  lock/swap/refill. Questions already heard by the expected players are left
  out. Printouts: host sheet, answer sheets, picture handout, presenter view and
  an answers CSV.

One deployment serves one group. The code is public; your bank lives only in
your own database.

**Stack:** Rust, axum + maud + htmx (server-rendered, no Node), SQLite,
teloxide, all in one binary. Design: [`docs/DESIGN.md`](docs/DESIGN.md).
Deploying: [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).

## Development

```bash
cargo test                       # unit + integration tests (in-memory SQLite, fake Telegram)
cargo llvm-cov --summary-only    # coverage
TELEGRAM_GROUP_ID=-1 cargo run   # web app on http://127.0.0.1:8090, no bot without a token
```

## License

MIT
