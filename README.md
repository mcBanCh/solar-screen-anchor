# solar_screen (Anchor)

SOL escrow for AI call-screening jobs run on solar-powered nodes. Hackathon prototype, **not audited**. Built with AI assistance.

Flow: `initialize` → `register_node` (0.05 SOL stake on node PDA) → `create_job` (escrow on job PDA) → `claim_job` → `submit_result` → `settle` (95% node / 5% treasury) or `cancel_job` (refund while Open).

## Authorization
- Node PDA `["node", operator]` + `has_one = operator`; job PDA `["job", payer, id]` re-derived on every instruction.
- `submit_result` / `settle`: `job.node == node` (`has_one = node`), so a payout can't be sent to another node.
- `settle`: signer must be the node operator or the config authority. Payout accounts are pinned by `address = node.operator` / `config.treasury`; checked math.
- `cancel_job`: `has_one = payer`, only while `Open`.

## Build & test
```
cargo build-sbf --arch v0
cargo test -p solar_screen --test security   # LiteSVM, 6 tests incl. negative cases
anchor test                                   # TS test in tests/ (needs Anchor CLI 0.31)
```
