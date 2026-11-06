# Coverage

What has been checked against what. The column that matters is the one on the right: a request's form
is proven only by something other than this crate agreeing with it.

- **SDK**: byte-identical to the official Python SDK's own output (hash, signature, and body for L1).
- **Live**: the real testnet exchange recovered the expected signer from a throwaway key, which proves
  the server's re-serialization hashes the same bytes. This is how actions with no SDK support were
  checked: their field order came only from the docs, and a wrong order would recover a stranger.
- **Reads** are live against mainnet: a wrong field name answers 422.

Run them with `ht conformance` and `ht conformance-read`. Neither can move funds.

## L1 actions

| Action | SDK | Live |
|--------|-----|------|
| `order`, with cloid, builder, trigger and two orders | ✓ | ✓ |
| `order`, with `{"p": rate}` priority grouping | | ✓ |
| `order`, with expiry, vault, and both | ✓ | ✓ |
| `cancel` | ✓ | ✓ |
| `cancel`, fast | | ✓ |
| `cancelByCloid` | ✓ | ✓ |
| `scheduleCancel` | ✓ | ✓ |
| `batchModify` | ✓ | ✓ |
| `modify`, with `a` (always place) | | ✓ |
| `updateLeverage`, `updateIsolatedMargin` | ✓ | ✓ |
| `twapOrder`, `twapCancel`, `trailingStop` | | ✓ |
| `vaultTransfer`, `subAccountTransfer`, `subAccountSpotTransfer` | ✓ | ✓ |
| `hip3LiquidatorTransfer` | | ✓ |
| `createSubAccount`, `setReferrer`, `evmUserModify` | ✓ | ✓ |
| `noop` | ✓ | ✓ |
| `noop`, with a signature scalar short a leading digit | | ✓ |
| `claimRewards`, `reserveRequestWeight`, `cDeposit2` | | ✓ |
| `gossipPriorityBid`, `agentSetAbstraction` | ✓ | ✓ |
| `agentSendAsset`, `userOutcome` (all four) | | ✓ |
| `agentEnableDexAbstraction` | ✓ | **422**, and from the SDK too |
| `multiSig`, inner L1 payload and envelope | ✓ | |

## User-signed actions

| Action | SDK | Live |
|--------|-----|------|
| `usdSend`, `spotSend`, `withdraw3`, `usdClassTransfer`, `sendAsset` | ✓ | ✓ |
| `tokenDelegate`, `approveAgent`, `approveBuilderFee`, `convertToMultiSigUser` | ✓ | ✓ |
| `userSetAbstraction` | ✓ | ✓ |
| `cDeposit`, `cWithdraw`, `sendToEvmWithData` | | ✓ |
| `userDexAbstraction` | ✓ | **422**, and from the SDK too |

The two 422s are the deprecated HIP-3 dex abstraction, which the docs replace with `userSetAbstraction`.
The server refuses the official SDK identically, so they are kept for accounts that need them, and the
question of whether their encoding is right is one nothing can currently answer.

## Not typed

Deployer and validator actions (`perpDeploy`, `spotDeploy`, `CSignerAction`, `CValidatorAction`,
`authorizeAqav2Role`, `validatorL1Stream`). They are few, rarely used and large. Implement `Walk` for
one and it signs through `sign_l1` like any other.

## Reads

44 `/info` requests were sent to mainnet; 43 answered 200 with parseable JSON. `userDexAbstraction`
answers 422, the same deprecation as above. All 24 subscriptions were acknowledged.

A 200 proves the request's form, not this crate's reading of the answer. **Typed readers exist for** books,
BBO, trades, candles, asset contexts, fills, order updates, order status, open orders, positions, account
summary, rate limits and `/exchange` replies, and each is pinned to captured traffic in
`tests/live_fixtures.rs`. Everything else is reachable through the tape, untyped: `value.get("field")`.

## Not handled

- `fastAssetCtxs` arrives base64 and DEFLATE-compressed. The subscription works, and the `no_std` kernel
  has no inflate, so its payload is the caller's to decode.
- `Subscription` and `Info` cover what the docs list as of 2026-11-05, and `Info::Bare` and
  `Info::ForUser` reach anything added later that takes no arguments or only a user.
