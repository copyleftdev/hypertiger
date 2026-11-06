//! A small command line over the crate: read the live exchange, and prove the signer against it.
//!
//! Nothing here can move funds. `conformance` and `e2e` run only on testnet, and only with a key
//! generated for that run, which has no funds and is never registered with anything.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use hypertiger::action::{
    Action, AssetTransfer, Builder, CancelCloidWire, CancelWire, Envelope, EvmTransfer, Grouping,
    L1Options, ModifyWire, OrderKind, OrderRef, OrderWire, OutcomeAction, Retracement, Tif,
    TwapWire, UserAction, sign_l1,
};
use hypertiger::book::Book;
use hypertiger::decimal::{Px, Sz};
use hypertiger::engine::{Engine, EngineConfig, Event, Intent, MarketConfig};
use hypertiger::feed::{Channel, Message, PerpInfo};
use hypertiger::info::{Info, Subscription};
use hypertiger::json::{Token, Value, parse};
use hypertiger::market::{Asset, Market};
use hypertiger::signing::{Signer, Wallet};
use hypertiger::sink::{JsonSink, Walk};
use hypertiger::types::{Address, Cloid, Coin, Network, Side};
use hypertiger_net::conn::HttpClient;
use hypertiger_net::{
    HOST_MAINNET, HOST_TESTNET, Host, Session, SessionConfig, now_ms, random_u64,
};

const USAGE: &str = "\
usage: ht <command>

  book <coin> [--testnet]            print the top of the live book
  watch <coin> [--seconds N] [--testnet]
                                     stream the book and count messages
  markets [--testnet]                list perp markets
  conformance                        have the testnet exchange recover the signer of every action
  conformance-read [--testnet]       send every read request and subscription, and read the answers
  e2e [--seconds N]                  run the whole engine against testnet with a throwaway key";

const RESPONSE_SIZE_MAX: usize = 4 * 1024 * 1024;
const TOKENS_MAX: usize = 512 * 1024;
const TIMEOUT_MS: u64 = 15_000;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = match arguments.first().map(String::as_str) {
        Some("book") => run_book(&arguments[1..]),
        Some("watch") => run_watch(&arguments[1..]),
        Some("markets") => run_markets(&arguments[1..]),
        Some("conformance") => run_conformance(),
        Some("conformance-read") => run_conformance_read(&arguments[1..]),
        Some("e2e") => run_e2e(&arguments[1..]),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn host_of(arguments: &[String]) -> Host {
    if arguments.iter().any(|argument| argument == "--testnet") {
        HOST_TESTNET
    } else {
        HOST_MAINNET
    }
}

fn number_flag(arguments: &[String], name: &str, default: u64) -> Result<u64, String> {
    let Some(position) = arguments.iter().position(|argument| argument == name) else {
        return Ok(default);
    };
    let text = arguments.get(position + 1).ok_or_else(|| format!("{name} needs a number"))?;
    text.parse().map_err(|_| format!("{name}: {text} is not a number"))
}

/// Client memory for one-shot requests, allocated once per command.
struct Rest {
    client: HttpClient,
    response: Vec<u8>,
    tokens: Vec<Token>,
    body: Vec<u8>,
}

impl Rest {
    fn new(host: Host) -> Rest {
        Rest {
            client: HttpClient::new(host),
            response: vec![0; RESPONSE_SIZE_MAX],
            tokens: vec![Token::ZERO; TOKENS_MAX],
            body: vec![0; 64 * 1024],
        }
    }

    /// Posts a walkable body and hands back the status and raw reply bytes.
    fn post<W: Walk>(&mut self, path: &str, value: &W) -> Result<(u16, Vec<u8>), String> {
        let mut sink = JsonSink::new(&mut self.body);
        value.walk(&mut sink);
        let size = sink.finish().map_err(|_| "request body too large".to_string())?;
        let response = self
            .client
            .post(path, &self.body[..size], &mut self.response, TIMEOUT_MS)
            .map_err(|error| format!("{path}: {error}"))?;
        let body = &self.response[response.body_start..response.body_start + response.body_size];
        Ok((response.status, body.to_vec()))
    }

    fn info<R>(
        &mut self,
        request: &Info<'_>,
        read: impl FnOnce(Value<'_, '_>) -> Result<R, String>,
    ) -> Result<R, String> {
        let (status, bytes) = self.post("/info", request)?;
        if status != 200 {
            return Err(format!("info returned {status}: {}", String::from_utf8_lossy(&bytes)));
        }
        let document =
            parse(&bytes, &mut self.tokens).map_err(|error| format!("reply: {error:?}"))?;
        read(document.root())
    }
}

fn print_book(book: &Book, coin: &str) {
    println!(
        "{coin} at {} ms   mid {:?}",
        book.time_ms(),
        book.mid().map(hypertiger::decimal::Px::to_wire)
    );
    for level in book.asks().iter().take(5).rev() {
        println!("  ask {:>14} {:>16} ({})", level.px, level.sz, level.orders_count);
    }
    for level in book.bids().iter().take(5) {
        println!("  bid {:>14} {:>16} ({})", level.px, level.sz, level.orders_count);
    }
}

fn run_book(arguments: &[String]) -> Result<(), String> {
    let coin = arguments.first().ok_or_else(|| USAGE.to_string())?;
    let mut rest = Rest::new(host_of(arguments));
    let request = Info::L2Book { coin, significant_figures: None, mantissa: None };
    let book = rest.info(&request, |root| {
        let mut book = Book::EMPTY;
        book.apply(root).map_err(|error| format!("book: {error:?}"))?;
        Ok(book)
    })?;
    print_book(&book, coin);
    Ok(())
}

fn run_markets(arguments: &[String]) -> Result<(), String> {
    let mut rest = Rest::new(host_of(arguments));
    rest.info(&Info::Meta { dex: None }, |root| {
        let universe = root.get("universe").ok_or("no universe")?;
        for (index, item) in universe.items().enumerate() {
            let market = PerpInfo::parse(item).ok_or("a market could not be read")?;
            if !market.is_delisted {
                println!(
                    "{index:>5} {:<12} szDecimals {} maxLeverage {}",
                    market.coin.as_str(),
                    market.sz_decimals,
                    market.leverage_max
                );
            }
        }
        Ok(())
    })
}

fn throwaway_wallet() -> Wallet {
    // Many draws of the hash seed, then Keccak, because one draw is 64 bits and a key is 256. The
    // key guards nothing: it holds no funds and lives only as long as this command.
    let mut seed = [0u8; 64];
    for chunk in seed.chunks_exact_mut(8) {
        chunk.copy_from_slice(&random_u64().to_be_bytes());
    }
    match Wallet::from_bytes(&hypertiger::signing::keccak(&seed)) {
        Some(wallet) => wallet,
        None => panic!("a hash output is an invalid scalar with negligible probability"),
    }
}

fn run_watch(arguments: &[String]) -> Result<(), String> {
    let coin = arguments.first().ok_or_else(|| USAGE.to_string())?;
    let seconds = number_flag(arguments, "--seconds", 10)?;
    let host = host_of(arguments);
    let network = if host == HOST_TESTNET { Network::Testnet } else { Network::Mainnet };
    let wallet = throwaway_wallet();
    let user = wallet.address();

    let coin_inline = Coin::parse(coin.as_bytes()).ok_or("coin name")?;
    let market = Market::new(Asset::perp(0), 0).ok_or("market")?;
    let markets = [market_config(coin_inline, market)];
    let engine: Box<Engine<8, 1>> =
        Box::new(Engine::new(watch_config(network, user), &markets, now_ms()));
    let mut session = Session::new(SessionConfig { host, user }, engine, wallet);

    let mut book = Book::EMPTY;
    let mut updates = 0u64;
    let mut subscribed = false;
    let deadline_ms = now_ms() + seconds * 1000;
    while now_ms() < deadline_ms {
        session.step(&mut |message: &Message<'_, '_>| {
            if message.channel == Channel::L2Book {
                if let Some(data) = message.data {
                    if book.apply(data).is_ok() {
                        updates += 1;
                    }
                }
            }
        });
        if session.is_connected() && !subscribed {
            let subscription = Subscription::L2Book {
                coin,
                significant_figures: None,
                mantissa: None,
                fast: false,
            };
            session.subscribe(subscription).map_err(|error| error.to_string())?;
            subscribed = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    print_book(&book, coin);
    println!(
        "{updates} book updates, {} messages, {} connects",
        session.messages, session.connects
    );
    if updates == 0 {
        return Err("no book updates arrived".to_string());
    }
    Ok(())
}

fn market_config(coin: Coin, market: Market) -> MarketConfig {
    let limit = Sz::from_raw(u128::from(hypertiger::decimal::WIRE_ONE));
    MarketConfig { coin, market, long_sz_max: limit, short_sz_max: limit }
}

fn watch_config(network: Network, user: Address) -> EngineConfig {
    EngineConfig {
        network,
        user,
        vault: None,
        tick_ms: 100,
        order_ttl_ms: 5_000,
        settle_margin_ms: 2_000,
        request_timeout_ms: 3_000,
        queue_ms_max: 2_000,
        status_poll_ms: 1_000,
        reconcile_ms: 30_000,
        fills_wait_ms: 5_000,
        weight_per_minute: 600,
        weight_burst: 200,
        weight_cancel_reserve: 50,
        fast_cancel: true,
    }
}

// --- Conformance -------------------------------------------------------------------------------

/// The result of one action against the live exchange.
enum Verdict {
    /// The server recovered the signer we expected: our bytes and its bytes hash the same.
    SignerMatches(String),
    /// The server recovered a different address: the encoding is wrong.
    SignerDiffers(String),
    /// The server processed the action far enough to answer without naming a signer.
    Answered(String),
}

/// Reads the two refusals that name the recovered signer. Any other address in a reply, such as a
/// sub-account the action itself named, says nothing about the signature.
fn verdict_of(status: u16, text: &str, expected: Address) -> Verdict {
    let lower = text.to_ascii_lowercase();
    let named_after = ["user or api wallet ", "must deposit before performing actions. user: "]
        .iter()
        .find_map(|phrase| lower.find(phrase).map(|at| at + phrase.len()));
    let Some(found) = named_after.and_then(|at| lower.get(at..at + 42)) else {
        return Verdict::Answered(format!("{status} {}", short(text)));
    };
    let expected_hex = expected.to_hex();
    if found == expected_hex.as_str() {
        Verdict::SignerMatches(short(text))
    } else {
        Verdict::SignerDiffers(format!("expected {}: {}", expected_hex.as_str(), short(text)))
    }
}

fn short(text: &str) -> String {
    text.chars().take(110).collect()
}

fn run_conformance() -> Result<(), String> {
    let mut rest = Rest::new(HOST_TESTNET);
    let wallet = throwaway_wallet();
    println!("throwaway signer {}\n", wallet.address());
    let (mut matched, mut differed, mut answered) = (0u32, 0u32, 0u32);
    let mut nonce = now_ms();
    let mut report = |name: &str, verdict: Verdict| match verdict {
        Verdict::SignerMatches(detail) => {
            matched += 1;
            println!("  ok    {name:<26} {detail}");
        }
        Verdict::SignerDiffers(detail) => {
            differed += 1;
            println!("  WRONG {name:<26} {detail}");
        }
        Verdict::Answered(detail) => {
            answered += 1;
            println!("  ?     {name:<26} {detail}");
        }
    };

    let probe = ConformanceData::new(wallet.address());
    for name in L1_NAMES {
        nonce += 1;
        let verdict = probe
            .with_l1(name, nonce, &mut |action| send_l1(&mut rest, &wallet, action, nonce))??;
        report(name, verdict);
    }
    for name in USER_NAMES {
        nonce += 1;
        let verdict =
            probe.with_user(name, nonce, &mut |action| send_user(&mut rest, &wallet, action))??;
        report(name, verdict);
    }
    for (name, verdict) in envelope_variants(&mut rest, &wallet, &probe, &mut nonce)? {
        report(name, verdict);
    }
    println!(
        "\n{matched} recovered our signer, {answered} answered without naming one, {differed} recovered another"
    );
    if differed > 0 {
        return Err("the exchange recovered a different signer: an encoding is wrong".to_string());
    }
    Ok(())
}

fn send_l1(
    rest: &mut Rest,
    wallet: &Wallet,
    action: &Action<'_>,
    nonce: u64,
) -> Result<Verdict, String> {
    let options =
        L1Options { network: Network::Testnet, nonce, vault: None, expires_after_ms: None };
    let signature = sign_l1(action, &options, wallet);
    let envelope = Envelope { action, nonce, signature, vault: None, expires_after_ms: None };
    let (status, bytes) = rest.post("/exchange", &envelope)?;
    Ok(verdict_of(status, &String::from_utf8_lossy(&bytes), wallet.address()))
}

/// The parts of an envelope a plain action does not exercise: an expiry, a vault, and scalars short
/// enough to lose a leading digit, which the wire form strips as the reference does.
fn envelope_variants(
    rest: &mut Rest,
    wallet: &Wallet,
    probe: &ConformanceData,
    nonce: &mut u64,
) -> Result<Vec<(&'static str, Verdict)>, String> {
    let limit = probe.order(OrderKind::Limit(Tif::Alo));
    let orders = [limit];
    let action = Action::Order { orders: &orders, grouping: Grouping::Na, builder: None };
    let mut results = Vec::new();

    *nonce += 1;
    let expires = Some(now_ms() + 30_000);
    results
        .push(("order_with_expiry", send_variant(rest, wallet, &action, *nonce, None, expires)?));
    *nonce += 1;
    let vault = Some(probe.other);
    results.push(("order_with_vault", send_variant(rest, wallet, &action, *nonce, vault, None)?));
    *nonce += 1;
    results
        .push(("order_vault_expiry", send_variant(rest, wallet, &action, *nonce, vault, expires)?));

    for (name, take) in [
        (
            "noop_short_r",
            (|signature| signature.r) as fn(&hypertiger::signing::Signature) -> [u8; 32],
        ),
        ("noop_short_s", |signature| signature.s),
    ] {
        let short_nonce = grind_short_scalar(wallet, nonce, take);
        results.push((name, send_l1(rest, wallet, &Action::Noop, short_nonce)?));
    }
    Ok(results)
}

fn send_variant(
    rest: &mut Rest,
    wallet: &Wallet,
    action: &Action<'_>,
    nonce: u64,
    vault: Option<Address>,
    expires_after_ms: Option<u64>,
) -> Result<Verdict, String> {
    let options = L1Options { network: Network::Testnet, nonce, vault, expires_after_ms };
    let signature = sign_l1(action, &options, wallet);
    let envelope = Envelope { action, nonce, signature, vault, expires_after_ms };
    let (status, bytes) = rest.post("/exchange", &envelope)?;
    Ok(verdict_of(status, &String::from_utf8_lossy(&bytes), wallet.address()))
}

/// Tries nonces until a signature's scalar has a leading zero digit: one in 16, on average.
fn grind_short_scalar(
    wallet: &Wallet,
    nonce: &mut u64,
    take: fn(&hypertiger::signing::Signature) -> [u8; 32],
) -> u64 {
    for _ in 0..10_000 {
        *nonce += 1;
        let options = L1Options {
            network: Network::Testnet,
            nonce: *nonce,
            vault: None,
            expires_after_ms: None,
        };
        let signature = sign_l1(&Action::Noop, &options, wallet);
        if take(&signature)[0] < 0x10 {
            return *nonce;
        }
    }
    panic!("no short scalar in 10,000 signatures, which has probability 16^-640 or so");
}

fn send_user(rest: &mut Rest, wallet: &Wallet, action: &UserAction<'_>) -> Result<Verdict, String> {
    let signature = action.sign(wallet);
    let envelope = Envelope {
        action,
        nonce: action.nonce_ms(),
        signature,
        vault: None,
        expires_after_ms: None,
    };
    let (status, bytes) = rest.post("/exchange", &envelope)?;
    Ok(verdict_of(status, &String::from_utf8_lossy(&bytes), wallet.address()))
}

const L1_NAMES: &[&str] = &[
    "order",
    "order_builder",
    "order_trigger",
    "order_priority",
    "cancel",
    "cancel_fast",
    "cancel_cloid",
    "schedule_cancel",
    "modify",
    "modify_always_place",
    "batch_modify",
    "update_leverage",
    "update_margin",
    "twap_order",
    "twap_cancel",
    "trailing_stop",
    "vault_transfer",
    "sub_account_transfer",
    "sub_account_spot_transfer",
    "hip3_transfer",
    "create_sub_account",
    "set_referrer",
    "evm_user_modify",
    "noop",
    "claim_rewards",
    "reserve_weight",
    "gossip_bid",
    "c_deposit2",
    "agent_send_asset",
    "agent_dex_abstraction",
    "agent_set_abstraction",
    "outcome_split",
    "outcome_merge",
    "outcome_merge_question",
    "outcome_negate",
];

const USER_NAMES: &[&str] = &[
    "usd_send",
    "spot_send",
    "withdraw",
    "usd_class_transfer",
    "send_asset",
    "send_to_evm",
    "c_deposit",
    "c_withdraw",
    "token_delegate",
    "approve_agent",
    "approve_builder_fee",
    "convert_multi_sig",
    "user_dex_abstraction",
    "user_set_abstraction",
];

struct ConformanceData {
    /// The signer, for the actions that must name the account they act on.
    signer: Address,
    px: Px,
    sz: Sz,
    other: Address,
    cloid: Cloid,
    asset: Asset,
}

impl ConformanceData {
    fn new(signer: Address) -> ConformanceData {
        ConformanceData {
            signer,
            px: Px::from_raw(1000 * hypertiger::decimal::WIRE_ONE),
            sz: Sz::from_raw(u128::from(hypertiger::decimal::WIRE_ONE)),
            other: Address([0x11; 20]),
            cloid: Cloid::from_parts(1, 2),
            asset: Asset::perp(0),
        }
    }

    fn order(&self, kind: OrderKind) -> OrderWire {
        OrderWire {
            asset: self.asset,
            side: Side::Buy,
            px: self.px,
            sz: self.sz,
            reduce_only: false,
            kind,
            cloid: Some(self.cloid),
        }
    }

    fn with_l1<R>(
        &self,
        name: &str,
        nonce: u64,
        send: &mut dyn FnMut(&Action<'_>) -> R,
    ) -> Result<R, String> {
        let limit = self.order(OrderKind::Limit(Tif::Gtc));
        let orders = [limit];
        let cancels = [CancelWire { asset: self.asset, oid: 1 }];
        let cancels_cloid = [CancelCloidWire { asset: self.asset, cloid: self.cloid }];
        let modifies = [ModifyWire { target: OrderRef::Oid(1), order: limit }];
        let trigger = [OrderWire {
            reduce_only: true,
            ..self.order(OrderKind::Trigger {
                is_market: true,
                trigger_px: self.px,
                tpsl: hypertiger::action::Tpsl::Sl,
            })
        }];
        let builder = Builder { address: self.other, fee: 1 };
        let action = match name {
            "order" => Action::Order { orders: &orders, grouping: Grouping::Na, builder: None },
            "order_builder" => {
                Action::Order { orders: &orders, grouping: Grouping::Na, builder: Some(builder) }
            }
            "order_trigger" => {
                Action::Order { orders: &trigger, grouping: Grouping::Na, builder: None }
            }
            "order_priority" => {
                Action::Order { orders: &orders, grouping: Grouping::Priority(1000), builder: None }
            }
            "cancel" => Action::Cancel { cancels: &cancels, fast: false },
            "cancel_fast" => Action::Cancel { cancels: &cancels, fast: true },
            "cancel_cloid" => Action::CancelByCloid { cancels: &cancels_cloid, fast: true },
            "modify" => {
                Action::Modify { target: OrderRef::Oid(1), order: limit, always_place: false }
            }
            "modify_always_place" => Action::Modify {
                target: OrderRef::Cloid(self.cloid),
                order: limit,
                always_place: true,
            },
            "batch_modify" => Action::BatchModify { modifies: &modifies },
            _ => self.flat_action(name, nonce)?,
        };
        Ok(send(&action))
    }

    fn flat_action(&self, name: &str, nonce: u64) -> Result<Action<'static>, String> {
        let (asset, other, sz, px) = (self.asset, self.other, self.sz, self.px);
        Ok(match name {
            "schedule_cancel" => Action::ScheduleCancel { time_ms: Some(now_ms() + 60_000) },
            "update_leverage" => Action::UpdateLeverage { asset, is_cross: true, leverage: 2 },
            "update_margin" => Action::UpdateIsolatedMargin { asset, ntli: -1_000_000 },
            "twap_order" => Action::TwapOrder {
                twap: TwapWire {
                    asset,
                    side: Side::Buy,
                    sz,
                    reduce_only: false,
                    minutes: 5,
                    randomize: false,
                },
            },
            "twap_cancel" => Action::TwapCancel { asset, twap_id: 1 },
            "trailing_stop" => Action::TrailingStop {
                asset,
                side: Side::Sell,
                sz,
                reduce_only: true,
                retracement: Retracement::Px(px),
                activation_px: None,
            },
            "vault_transfer" => Action::VaultTransfer { vault: other, is_deposit: true, usd: 1 },
            "sub_account_transfer" => {
                Action::SubAccountTransfer { sub_account: other, is_deposit: true, usd: 1 }
            }
            "sub_account_spot_transfer" => Action::SubAccountSpotTransfer {
                sub_account: other,
                is_deposit: true,
                token: "USDC",
                amount: sz,
            },
            "hip3_transfer" => {
                Action::Hip3LiquidatorTransfer { dex: "test", ntl: 1, is_deposit: true }
            }
            "create_sub_account" => Action::CreateSubAccount { name: "hypertiger" },
            "set_referrer" => Action::SetReferrer { code: "HYPERTIGER" },
            "evm_user_modify" => Action::EvmUserModify { using_big_blocks: false },
            "noop" => Action::Noop,
            "claim_rewards" => Action::ClaimRewards,
            "reserve_weight" => Action::ReserveRequestWeight { weight: 1, destination: None },
            "gossip_bid" => Action::GossipPriorityBid { slot_id: 0, ip: "127.0.0.1", max_gas: 1 },
            "c_deposit2" => Action::CDeposit2 { wei: 1 },
            "agent_send_asset" => Action::AgentSendAsset {
                destination: other,
                source_dex: "",
                destination_dex: "spot",
                token: "USDC",
                amount: sz,
                from_sub_account: None,
                nonce_ms: nonce,
            },
            "agent_dex_abstraction" => Action::AgentEnableDexAbstraction,
            "agent_set_abstraction" => Action::AgentSetAbstraction { abstraction: "i" },
            "outcome_split" => Action::UserOutcome(OutcomeAction::Split { outcome: 1, amount: sz }),
            "outcome_merge" => Action::UserOutcome(OutcomeAction::Merge { outcome: 1, amount: sz }),
            "outcome_merge_question" => {
                Action::UserOutcome(OutcomeAction::MergeQuestion { question: 1, amount: sz })
            }
            "outcome_negate" => {
                Action::UserOutcome(OutcomeAction::Negate { question: 1, outcome: 1, amount: sz })
            }
            other => return Err(format!("no action named {other}")),
        })
    }

    fn with_user<R>(
        &self,
        name: &str,
        nonce: u64,
        send: &mut dyn FnMut(&UserAction<'_>) -> R,
    ) -> Result<R, String> {
        let network = Network::Testnet;
        let (other, sz) = (self.other, self.sz);
        let transfer = EvmTransfer {
            token: "USDC",
            amount: sz,
            source_dex: "",
            destination_recipient: "0x1111111111111111111111111111111111111111",
            address_encoding: "hex",
            destination_chain_id: 1,
            gas_limit: 21_000,
            data: &[0xde, 0xad],
        };
        let action = match name {
            "usd_send" => UserAction::usd_send(network, other, sz, nonce),
            "spot_send" => UserAction::spot_send(network, other, "USDC", sz, nonce),
            "withdraw" => UserAction::withdraw(network, other, sz, nonce),
            "usd_class_transfer" => UserAction::usd_class_transfer(network, "1", true, nonce),
            "send_asset" => UserAction::send_asset(
                network,
                &AssetTransfer {
                    destination: other,
                    source_dex: "",
                    destination_dex: "spot",
                    token: "USDC",
                    amount: sz,
                    from_sub_account: None,
                },
                nonce,
            ),
            "send_to_evm" => UserAction::send_to_evm_with_data(network, &transfer, nonce),
            "c_deposit" => UserAction::c_deposit(network, 1, nonce),
            "c_withdraw" => UserAction::c_withdraw(network, 1, nonce),
            "token_delegate" => UserAction::token_delegate(network, other, 1, false, nonce),
            "approve_agent" => UserAction::approve_agent(network, other, "bot", nonce),
            "approve_builder_fee" => {
                UserAction::approve_builder_fee(network, "0.001%", other, nonce)
            }
            "convert_multi_sig" => UserAction::convert_to_multi_sig(
                network,
                r#"{"authorizedUsers":["0x1111111111111111111111111111111111111111"],"threshold":1}"#,
                nonce,
            ),
            "user_dex_abstraction" => {
                UserAction::user_dex_abstraction(network, self.signer, true, nonce)
            }
            "user_set_abstraction" => {
                UserAction::user_set_abstraction(network, self.signer, "disabled", nonce)
            }
            other => return Err(format!("no user action named {other}")),
        };
        Ok(send(&action))
    }
}

// --- Read conformance -------------------------------------------------------------------------

/// Every request the crate can build, against the real server. A wrong field name or shape answers
/// 422, so a 200 with parseable JSON means the request's form is right. It does not vouch for this
/// crate's typed readers, which `tests/live_fixtures.rs` holds against captured traffic.
fn run_conformance_read(arguments: &[String]) -> Result<(), String> {
    let host = host_of(arguments);
    let mut rest = Rest::new(host);
    // The protocol's public vault on mainnet: an account with state, and no secret involved.
    let user = Address::parse(b"0xdfc24b077bc1425ad1dea75bcb6f8158e10df303").ok_or("address")?;
    let mut failures = 0u32;
    let mut sent = 0u32;
    let now = now_ms();
    for request in info_requests(user, now) {
        let weight = request.weight();
        let (status, bytes) = rest.post("/info", &request)?;
        let parsed = parse(&bytes, &mut rest.tokens).is_ok();
        sent += 1;
        let good = status == 200 && parsed;
        failures += u32::from(!good);
        let name = format!("{request:?}");
        let name = name.split([' ', '{', '(']).next().unwrap_or_default().to_string();
        let mark = if good { "ok   " } else { "WRONG" };
        println!("  {mark} {name:<30} {status} weight {weight:>2} {} bytes", bytes.len());
        // The request budget is 1200 weight a minute per IP, and this is one IP's whole sweep.
        std::thread::sleep(std::time::Duration::from_millis(u64::from(weight) * 60));
    }
    println!("\n{sent} requests, {failures} refused");
    run_subscription_conformance(host, user)?;
    if failures > 0 {
        return Err(format!("{failures} requests were refused"));
    }
    Ok(())
}

fn info_requests(user: Address, now: u64) -> Vec<Info<'static>> {
    let hour = 3_600_000;
    let start = now - hour;
    vec![
        Info::AllMids { dex: None },
        Info::Meta { dex: None },
        Info::MetaAndAssetCtxs,
        Info::SpotMeta,
        Info::SpotMetaAndAssetCtxs,
        Info::PerpDexs,
        Info::ExchangeStatus,
        Info::L2Book { coin: "BTC", significant_figures: Some(5), mantissa: Some(2) },
        Info::CandleSnapshot {
            coin: "BTC",
            interval: "1m",
            time_start_ms: start,
            time_end_ms: now,
        },
        Info::FundingHistory { coin: "BTC", time_start_ms: start, time_end_ms: Some(now) },
        Info::RecentTrades { coin: "BTC" },
        Info::PerpsAtOpenInterestCap,
        Info::PredictedFundings,
        Info::OutcomeMeta,
        Info::ClearinghouseState { user, dex: None },
        Info::SpotClearinghouseState { user },
        Info::OpenOrders { user, dex: None },
        Info::FrontendOpenOrders { user, dex: None },
        Info::HistoricalOrders { user },
        Info::OrderStatus { user, target: OrderRef::Oid(1) },
        Info::OrderStatus { user, target: OrderRef::Cloid(Cloid::from_parts(1, 1)) },
        Info::UserFills { user, aggregate_by_time: true },
        Info::UserFillsByTime {
            user,
            time_start_ms: start,
            time_end_ms: Some(now),
            aggregate_by_time: false,
        },
        Info::UserFunding { user, time_start_ms: start, time_end_ms: Some(now) },
        Info::UserNonFundingLedgerUpdates { user, time_start_ms: start, time_end_ms: None },
        Info::UserRateLimit { user },
        Info::UserFees { user },
        Info::UserRole { user },
        Info::UserVaultEquities { user },
        Info::UserTwapSliceFills { user },
        Info::UserAbstraction { user },
        Info::UserDexAbstraction { user },
        Info::UserToMultiSigSigners { user },
        Info::SubAccounts { user },
        Info::ExtraAgents { user },
        Info::Portfolio { user },
        Info::Referral { user },
        Info::Delegations { user },
        Info::DelegatorSummary { user },
        Info::DelegatorHistory { user },
        Info::DelegatorRewards { user },
        Info::ActiveAssetData { user, coin: "BTC" },
        Info::VaultDetails { vault: user, user: None },
        Info::MaxBuilderFee { user, builder: user },
    ]
}

fn run_subscription_conformance(host: Host, user: Address) -> Result<(), String> {
    let coin = "BTC";
    let subscriptions = [
        Subscription::AllMids { dex: None },
        Subscription::L2Book { coin, significant_figures: None, mantissa: None, fast: false },
        Subscription::Trades { coin },
        Subscription::Bbo { coin },
        Subscription::Candle { coin, interval: "1m" },
        Subscription::ActiveAssetCtx { coin },
        Subscription::ActiveAssetData { user, coin },
        Subscription::Notification { user },
        Subscription::WebData3 { user },
        Subscription::OrderUpdates { user },
        Subscription::UserEvents { user },
        Subscription::UserFills { user, aggregate_by_time: false },
        Subscription::UserFundings { user },
        Subscription::UserNonFundingLedgerUpdates { user },
        Subscription::UserTwapSliceFills { user },
        Subscription::UserTwapHistory { user },
        Subscription::OpenOrders { user, dex: None },
        Subscription::ClearinghouseState { user, dex: None },
        Subscription::TwapStates { user, dex: None },
        Subscription::SpotState { user },
        Subscription::AllDexsClearinghouseState { user },
        Subscription::AllDexsAssetCtxs,
        Subscription::FastAssetCtxs,
        Subscription::OutcomeMetaUpdates,
    ];
    let wallet = throwaway_wallet();
    let network = if host == HOST_TESTNET { Network::Testnet } else { Network::Mainnet };
    let market = Market::new(Asset::perp(0), 0).ok_or("market")?;
    let markets = [market_config(Coin::parse(b"BTC").ok_or("coin")?, market)];
    let engine: Box<Engine<8, 1>> =
        Box::new(Engine::new(watch_config(network, user), &markets, now_ms()));
    let mut session = Session::new(SessionConfig { host, user }, engine, wallet);

    let mut acknowledged = 0u32;
    let mut errors = Vec::new();
    let mut channels = std::collections::BTreeSet::new();
    let mut next = 0usize;
    let deadline_ms = now_ms() + 20_000;
    while now_ms() < deadline_ms {
        session.step(&mut |message: &Message<'_, '_>| {
            channels.insert(message.channel_name.to_string());
            match message.channel {
                Channel::SubscriptionResponse => acknowledged += 1,
                Channel::Error => {
                    let mut decoded = [0u8; 512];
                    let size = message.data.and_then(|data| data.unescape(&mut decoded));
                    let text =
                        size.map_or("?".into(), |at| String::from_utf8_lossy(&decoded[..at]));
                    errors.push(short(&text));
                }
                _ => {}
            }
        });
        if session.is_connected() && next < subscriptions.len() {
            session.subscribe(subscriptions[next]).map_err(|error| error.to_string())?;
            next += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    println!("\n{acknowledged} of {} subscriptions acknowledged", subscriptions.len());
    println!("channels seen: {}", channels.into_iter().collect::<Vec<_>>().join(" "));
    for error in &errors {
        println!("  refused: {error}");
    }
    Ok(())
}

// --- End to end --------------------------------------------------------------------------------

/// Runs the real engine through the real socket against the real testnet exchange. The key has no
/// funds, so the exchange must refuse the order, and that refusal has to come back through every
/// layer as a proper `Finished { Rejected }`. A throwaway account can prove the path, not the
/// fills.
fn run_e2e(arguments: &[String]) -> Result<(), String> {
    let seconds = number_flag(arguments, "--seconds", 30)?;
    let wallet = throwaway_wallet();
    let user = wallet.address();
    let (coin, sz_decimals, market_px) = testnet_market()?;
    println!("throwaway account {user}, trading {coin} at about {market_px}");

    let market = Market::new(Asset::perp(0), sz_decimals).ok_or("market")?;
    let limit = Sz::from_raw(1000 * u128::from(hypertiger::decimal::WIRE_ONE));
    let markets = [MarketConfig {
        coin: Coin::parse(coin.as_bytes()).ok_or("coin")?,
        market,
        long_sz_max: limit,
        short_sz_max: limit,
    }];
    let engine: Box<Engine<16, 1>> =
        Box::new(Engine::new(watch_config(Network::Testnet, user), &markets, now_ms()));
    let mut session = Session::new(SessionConfig { host: HOST_TESTNET, user }, engine, wallet);

    let px = market.px_round(market_px, hypertiger::market::Round::Down).ok_or("px")?;
    let sz = market.sz_round_down(size_for_notional(px));
    let intent = Intent {
        cloid: Cloid::from_parts(random_u64(), 1),
        market_index: 0,
        side: Side::Buy,
        tif: Tif::Alo,
        px,
        sz,
        reduce_only: false,
    };

    let mut submitted = false;
    let mut finished = None;
    let deadline_ms = now_ms() + seconds * 1000;
    while now_ms() < deadline_ms && finished.is_none() {
        session.step(&mut |_message| {});
        if session.engine().is_running() && !submitted {
            session.engine_mut().submit(&intent, now_ms()).map_err(|error| format!("{error:?}"))?;
            println!("recovered from quarantine, order submitted");
            submitted = true;
        }
        while let Some(event) = session.poll_event() {
            println!("  event {event:?}");
            if let Event::Finished { finish, .. } = event {
                finished = Some(finish);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    println!("{} messages over {} connection(s)", session.messages, session.connects);
    match finished {
        Some(finish) => {
            println!("the order came back through every layer as {finish:?}");
            Ok(())
        }
        None => Err("the order never finished".to_string()),
    }
}

/// About 20 quote units' worth, so that the order clears the minimum and the exchange has to look
/// at the account to refuse it.
fn size_for_notional(px: Px) -> Sz {
    let units =
        20 * u128::from(hypertiger::decimal::WIRE_ONE) * u128::from(hypertiger::decimal::WIRE_ONE);
    Sz::from_raw(units / u128::from(px.raw()))
}

fn testnet_market() -> Result<(String, u8, Px), String> {
    let mut rest = Rest::new(HOST_TESTNET);
    let (coin, sz_decimals) = rest.info(&Info::Meta { dex: None }, |root| {
        let universe = root.get("universe").ok_or("no universe")?;
        let first = universe.items().next().ok_or("no markets")?;
        let market = PerpInfo::parse(first).ok_or("market unreadable")?;
        Ok((market.coin.as_str().to_string(), market.sz_decimals))
    })?;
    let request = Info::L2Book { coin: &coin, significant_figures: None, mantissa: None };
    let px = rest.info(&request, |root| {
        let mut book = Book::EMPTY;
        book.apply(root).map_err(|error| format!("book: {error:?}"))?;
        book.mid().ok_or_else(|| "the book is empty".to_string())
    })?;
    Ok((coin, sz_decimals, px))
}
