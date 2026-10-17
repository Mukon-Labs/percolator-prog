use litesvm::LiteSVM;
use percolator_martingaler_engine::martingaler::{
    V1_MAX_NET_PROFIT_PER_POSITION_QUOTE, V1_MAX_POSITION_NOTIONAL_QUOTE,
    V1_MAX_RESERVED_PROFIT_PER_CONTROLLER_QUOTE, V1_QUOTE_DECIMALS,
    V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
};
use percolator_prog::{
    dispatch_instruction, id,
    ix::Instruction as V16Instruction,
    martingaler_program::{
        derive_canonical_vault, derive_claim, derive_controller, derive_market,
        derive_oracle_policy, derive_pool, derive_position, derive_queue_page,
        derive_queue_page_zero, derive_vault_authority, has_namespace,
        Instruction as MartingalerInstruction, PopFifoShape, ProfitSettlementShape,
        CLOSE_CASH_POSITION_IX_LEN, CLOSE_CLAIM_POSITION_IX_LEN,
        CLOSE_PROFITABLE_CASH_POSITION_IX_LEN, CONTROLLER_IX_LEN, DEPOSIT_CASH_IX_LEN,
        INITIALIZE_MARKET_IX_LEN, INITIALIZE_ORACLE_POLICY_IX_LEN, INITIALIZE_VAULT_IX_LEN,
        MARTINGALER_IX_LEN, MARTINGALER_IX_MAGIC, MARTINGALER_IX_VERSION,
        OPEN_CASH_POSITION_IX_LEN, OPEN_CLAIM_POSITION_IX_LEN, POP_FIFO_HEAD_IX_LEN,
        PREPARE_CLAIM_IX_LEN, PREPARE_MARKET_IX_LEN, PREPARE_ORACLE_POLICY_IX_LEN,
        PREPARE_POSITION_IX_LEN, PREPARE_QUEUE_PAGE_IX_LEN, RESOLVE_BANKRUPT_CLAIM_POSITION_IX_LEN,
        SERVICE_FIFO_HEAD_IX_LEN, SET_STATUS_IX_LEN, WITHDRAW_IDLE_CASH_IX_LEN,
    },
    martingaler_state::{
        account_len, decode_account, decode_account_header, initialize_account, overwrite_account,
        validate_queue_page_zero_account, ClaimState, ClaimStatus, ControllerRiskState,
        FundingKind, HaircutConfig, Key32, MarketRiskState, OraclePolicyState, PoolState,
        PoolStatus, PositionSide, PositionState, PositionStatus, QueueEntry, QueuePageState,
        U128Le, U16Le, ACCOUNT_HEADER_LEN, NO_CLAIM_SEQUENCE, QUEUE_FLAG_HEAD_SETTLED,
        QUEUE_PAGE_CAPACITY,
    },
    oracle_v16, processor,
};
use solana_sdk::{
    account::Account,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction as SolInstruction},
    program_option::COption,
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    signature::{keypair_from_seed, Keypair, Signer},
    system_program,
    sysvar::clock::Clock,
    transaction::Transaction,
};
use spl_token::state::{Account as TokenAccount, AccountState, Mint};
use std::path::{Path, PathBuf};

const RISK_POOL_ID: u64 = 77;
const MARKET_ID: u16 = 7;
const DIVERGENCE_LIMIT_BPS: u16 = 500;
const TX_CU_LIMIT: u32 = 200_000;
const SOURCE_BALANCE: u64 = 1_000_000_000;
const DEPOSIT_AMOUNT: u64 = 250_000_000;
const POSITION_ID: u64 = 42;
const OPEN_COLLATERAL: u64 = 10_000_000;
const OPEN_NOTIONAL: u64 = 1_000_000_000;
const PROFIT_CLAIM_FACE: u64 = 8_820_000;
const PRIMARY_FEED_ID: [u8; 32] = [0x11; 32];
const SECONDARY_FEED_ID: [u8; 32] = [0x22; 32];
const ORACLE_TIME: i64 = 1_000;

fn haircut() -> HaircutConfig {
    HaircutConfig::new(100_000, 20, 2_000_000, 4_000_000, 100_000, 20_000)
}

struct Fixture {
    svm: LiteSVM,
    payer: Keypair,
    admin: Keypair,
    controller: Keypair,
    quote_mint: Pubkey,
    pool: Pubkey,
    page_zero: Pubkey,
    market: Pubkey,
    oracle_policy: Pubkey,
    controller_risk: Pubkey,
    position: Pubkey,
    vault_authority: Pubkey,
    vault: Pubkey,
    source: Pubkey,
    pool_bump: u8,
    page_bump: u8,
    market_bump: u8,
    oracle_policy_bump: u8,
    controller_bump: u8,
    position_bump: u8,
}

impl Fixture {
    fn new(seed: u8) -> Self {
        Self::new_with_mint(seed, V1_QUOTE_DECIMALS, true, spl_token::ID, Mint::LEN)
    }

    fn new_with_mint(
        seed: u8,
        decimals: u8,
        initialized: bool,
        mint_owner: Pubkey,
        mint_len: usize,
    ) -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program(
            id(),
            &std::fs::read(program_path()).expect("read exact Martingaler program SBF"),
        );
        svm.add_program(
            spl_token::ID,
            &std::fs::read(spl_token_program_path()).expect("read SPL Token SBF"),
        );
        svm.add_program(
            associated_token_program_id(),
            &std::fs::read(associated_token_program_path()).expect("read Associated Token SBF"),
        );

        let payer = deterministic_keypair(seed, 1);
        let admin = deterministic_keypair(seed, 2);
        let quote_mint = deterministic_keypair(seed, 3).pubkey();
        let controller = deterministic_keypair(seed, 4);
        let source = deterministic_keypair(seed, 5).pubkey();
        svm.airdrop(&payer.pubkey(), 10_000_000_000)
            .expect("fund fixture fee payer");
        svm.airdrop(&admin.pubkey(), 10_000_000_000)
            .expect("fund fixture pool admin");
        svm.airdrop(&controller.pubkey(), 10_000_000_000)
            .expect("fund fixture controller");

        let mut mint_data = vec![0u8; mint_len];
        if mint_len == Mint::LEN {
            Mint::pack(
                Mint {
                    mint_authority: COption::None,
                    supply: SOURCE_BALANCE,
                    decimals,
                    is_initialized: initialized,
                    freeze_authority: COption::None,
                },
                &mut mint_data,
            )
            .expect("pack quote mint fixture");
        }
        svm.set_account(
            quote_mint,
            Account {
                lamports: Rent::default().minimum_balance(mint_len),
                data: mint_data,
                owner: mint_owner,
                executable: false,
                rent_epoch: 0,
            },
        )
        .expect("install quote mint fixture");

        let (pool, pool_bump) = derive_pool(&id(), &admin.pubkey(), &quote_mint, RISK_POOL_ID);
        let (page_zero, page_bump) = derive_queue_page_zero(&id(), &pool);
        let (market, market_bump) = derive_market(&id(), &pool, MARKET_ID);
        let (oracle_policy, oracle_policy_bump) = derive_oracle_policy(&id(), &market);
        let (controller_risk, controller_bump) =
            derive_controller(&id(), &pool, &controller.pubkey());
        let (position, position_bump) =
            derive_position(&id(), &pool, &controller.pubkey(), POSITION_ID);
        let (vault_authority, _) = derive_vault_authority(&id(), &pool);
        let vault = derive_canonical_vault(&vault_authority, &quote_mint);
        if mint_owner == spl_token::ID && mint_len == Mint::LEN && initialized {
            let mut source_data = vec![0u8; TokenAccount::LEN];
            TokenAccount::pack(
                TokenAccount {
                    mint: quote_mint,
                    owner: controller.pubkey(),
                    amount: SOURCE_BALANCE,
                    delegate: COption::None,
                    state: AccountState::Initialized,
                    is_native: COption::None,
                    delegated_amount: 0,
                    close_authority: COption::None,
                },
                &mut source_data,
            )
            .expect("pack source token account");
            svm.set_account(
                source,
                Account {
                    lamports: Rent::default().minimum_balance(TokenAccount::LEN),
                    data: source_data,
                    owner: spl_token::ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .expect("install source token account");
        }
        Self {
            svm,
            payer,
            admin,
            controller,
            quote_mint,
            pool,
            page_zero,
            market,
            oracle_policy,
            controller_risk,
            position,
            vault_authority,
            vault,
            source,
            pool_bump,
            page_bump,
            market_bump,
            oracle_policy_bump,
            controller_bump,
            position_bump,
        }
    }

    fn metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.admin.pubkey(), true),
            AccountMeta::new_readonly(self.quote_mint, false),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.page_zero, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ]
    }

    fn instruction(
        &self,
        instruction: MartingalerInstruction,
        accounts: Vec<AccountMeta>,
    ) -> SolInstruction {
        SolInstruction {
            program_id: id(),
            accounts,
            data: instruction.encode().to_vec(),
        }
    }

    fn prepare(&self) -> SolInstruction {
        self.instruction(
            MartingalerInstruction::PreparePool {
                risk_pool_id: RISK_POOL_ID,
            },
            self.metas(),
        )
    }

    fn initialize(&self) -> SolInstruction {
        self.instruction(
            MartingalerInstruction::InitializePool {
                risk_pool_id: RISK_POOL_ID,
            },
            self.metas(),
        )
    }

    fn market_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.admin.pubkey(), true),
            AccountMeta::new_readonly(self.pool, false),
            AccountMeta::new(self.market, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ]
    }

    fn market_instruction(&self, prepare: bool) -> SolInstruction {
        let instruction = if prepare {
            MartingalerInstruction::PrepareMarket {
                market_id: MARKET_ID,
            }
        } else {
            MartingalerInstruction::InitializeMarket {
                market_id: MARKET_ID,
                divergence_limit_bps: DIVERGENCE_LIMIT_BPS,
                max_long_open_interest: V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
                max_short_open_interest: V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
                haircut: haircut(),
            }
        };
        self.instruction(instruction, self.market_metas())
    }

    fn controller_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.controller.pubkey(), true),
            AccountMeta::new_readonly(self.pool, false),
            AccountMeta::new(self.controller_risk, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ]
    }

    fn controller_instruction(&self, prepare: bool) -> SolInstruction {
        let instruction = if prepare {
            MartingalerInstruction::PrepareController
        } else {
            MartingalerInstruction::InitializeController
        };
        self.instruction(instruction, self.controller_metas())
    }

    fn vault_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.admin.pubkey(), true),
            AccountMeta::new_readonly(self.pool, false),
            AccountMeta::new_readonly(self.quote_mint, false),
            AccountMeta::new_readonly(self.vault_authority, false),
            AccountMeta::new(self.vault, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(associated_token_program_id(), false),
        ]
    }

    fn vault_instruction(&self) -> SolInstruction {
        self.instruction(MartingalerInstruction::InitializeVault, self.vault_metas())
    }

    fn deposit_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(self.controller.pubkey(), true),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.controller_risk, false),
            AccountMeta::new(self.source, false),
            AccountMeta::new(self.vault, false),
            AccountMeta::new_readonly(self.quote_mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ]
    }

    fn deposit_instruction(&self, amount: u64) -> SolInstruction {
        self.instruction(
            MartingalerInstruction::DepositCash { amount },
            self.deposit_metas(),
        )
    }

    fn set_pool_status_instruction(&self, target: PoolStatus) -> SolInstruction {
        self.instruction(
            MartingalerInstruction::SetPoolStatus { target },
            vec![
                AccountMeta::new_readonly(self.admin.pubkey(), true),
                AccountMeta::new(self.pool, false),
            ],
        )
    }

    fn set_market_status_instruction(&self, target: PoolStatus) -> SolInstruction {
        self.instruction(
            MartingalerInstruction::SetMarketStatus { target },
            vec![
                AccountMeta::new_readonly(self.admin.pubkey(), true),
                AccountMeta::new_readonly(self.pool, false),
                AccountMeta::new(self.market, false),
            ],
        )
    }

    fn withdraw_idle_cash_instruction(&self, amount: u64) -> SolInstruction {
        self.instruction(
            MartingalerInstruction::WithdrawIdleCash { amount },
            vec![
                AccountMeta::new_readonly(self.controller.pubkey(), true),
                AccountMeta::new(self.pool, false),
                AccountMeta::new(self.controller_risk, false),
                AccountMeta::new_readonly(self.vault_authority, false),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(self.source, false),
                AccountMeta::new_readonly(self.quote_mint, false),
                AccountMeta::new_readonly(spl_token::ID, false),
            ],
        )
    }

    fn oracle_policy_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.admin.pubkey(), true),
            AccountMeta::new_readonly(self.pool, false),
            AccountMeta::new_readonly(self.market, false),
            AccountMeta::new(self.oracle_policy, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ]
    }

    fn oracle_policy_instruction(&self, prepare: bool, dual: bool) -> SolInstruction {
        let instruction = if prepare {
            MartingalerInstruction::PrepareOraclePolicy {
                market_id: MARKET_ID,
            }
        } else {
            MartingalerInstruction::InitializeOraclePolicy {
                market_id: MARKET_ID,
                primary_feed_id: PRIMARY_FEED_ID,
                secondary_feed_id: if dual { SECONDARY_FEED_ID } else { [0; 32] },
                max_staleness_secs: 30,
                confidence_limit_bps: 100,
            }
        };
        self.instruction(instruction, self.oracle_policy_metas())
    }

    fn prepare_position_instruction(&self) -> SolInstruction {
        self.prepare_position_instruction_for(POSITION_ID)
    }

    fn position_for(&self, position_id: u64) -> Pubkey {
        derive_position(&id(), &self.pool, &self.controller.pubkey(), position_id).0
    }

    fn claim_for(&self, position_id: u64) -> Pubkey {
        derive_claim(&id(), &self.position_for(position_id)).0
    }

    fn prepare_position_instruction_for(&self, position_id: u64) -> SolInstruction {
        let position = self.position_for(position_id);
        self.instruction(
            MartingalerInstruction::PreparePosition { position_id },
            vec![
                AccountMeta::new(self.controller.pubkey(), true),
                AccountMeta::new_readonly(self.pool, false),
                AccountMeta::new_readonly(self.controller_risk, false),
                AccountMeta::new(position, false),
                AccountMeta::new_readonly(system_program::ID, false),
            ],
        )
    }

    fn open_cash_metas(
        &self,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut accounts = vec![
            AccountMeta::new(self.controller.pubkey(), true),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.market, false),
            AccountMeta::new(self.controller_risk, false),
            AccountMeta::new(self.position, false),
            AccountMeta::new_readonly(self.oracle_policy, false),
            AccountMeta::new_readonly(self.vault, false),
            AccountMeta::new_readonly(primary_oracle, false),
        ];
        if let Some(secondary_oracle) = secondary_oracle {
            accounts.push(AccountMeta::new_readonly(secondary_oracle, false));
        }
        accounts
    }

    fn open_cash_instruction(
        &self,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
        collateral: u64,
        notional: u64,
        leverage_x: u16,
    ) -> SolInstruction {
        self.open_cash_instruction_for(
            POSITION_ID,
            primary_oracle,
            secondary_oracle,
            collateral,
            notional,
            leverage_x,
        )
    }

    fn open_cash_instruction_for(
        &self,
        position_id: u64,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
        collateral: u64,
        notional: u64,
        leverage_x: u16,
    ) -> SolInstruction {
        let mut accounts = self.open_cash_metas(primary_oracle, secondary_oracle);
        accounts[4] = AccountMeta::new(self.position_for(position_id), false);
        self.instruction(
            MartingalerInstruction::OpenCashPosition {
                position_id,
                market_id: MARKET_ID,
                side: PositionSide::Long,
                collateral,
                notional,
                leverage_x,
            },
            accounts,
        )
    }

    fn close_cash_metas_with_oracles(
        &self,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut accounts = vec![
            AccountMeta::new_readonly(self.controller.pubkey(), true),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.market, false),
            AccountMeta::new(self.controller_risk, false),
            AccountMeta::new(self.position, false),
            AccountMeta::new_readonly(self.oracle_policy, false),
            AccountMeta::new_readonly(self.vault_authority, false),
            AccountMeta::new(self.vault, false),
            AccountMeta::new(self.source, false),
            AccountMeta::new_readonly(self.quote_mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(primary_oracle, false),
        ];
        if let Some(secondary_oracle) = secondary_oracle {
            accounts.push(AccountMeta::new_readonly(secondary_oracle, false));
        }
        accounts
    }

    fn close_cash_instruction(&self, primary_oracle: Pubkey) -> SolInstruction {
        self.close_cash_instruction_with_oracles(primary_oracle, None)
    }

    fn close_cash_instruction_with_oracles(
        &self,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
    ) -> SolInstruction {
        self.close_cash_instruction_for(POSITION_ID, primary_oracle, secondary_oracle)
    }

    fn close_cash_instruction_for(
        &self,
        position_id: u64,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
    ) -> SolInstruction {
        let mut accounts = self.close_cash_metas_with_oracles(primary_oracle, secondary_oracle);
        accounts[4] = AccountMeta::new(self.position_for(position_id), false);
        self.instruction(
            MartingalerInstruction::CloseNonPositiveCashPosition { position_id },
            accounts,
        )
    }

    fn claim(&self) -> Pubkey {
        derive_claim(&id(), &self.position).0
    }

    fn queue_page(&self, page_index: u64) -> Pubkey {
        derive_queue_page(&id(), &self.pool, page_index).0
    }

    fn prepare_claim_instruction(&self) -> SolInstruction {
        self.prepare_claim_instruction_for(POSITION_ID)
    }

    fn prepare_claim_instruction_for(&self, position_id: u64) -> SolInstruction {
        let position = self.position_for(position_id);
        self.instruction(
            MartingalerInstruction::PrepareClaim { position_id },
            vec![
                AccountMeta::new(self.controller.pubkey(), true),
                AccountMeta::new(position, false),
                AccountMeta::new(self.claim_for(position_id), false),
                AccountMeta::new_readonly(system_program::ID, false),
            ],
        )
    }

    fn prepare_claim_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.controller.pubkey(), true),
            AccountMeta::new(self.position, false),
            AccountMeta::new(self.claim(), false),
            AccountMeta::new_readonly(system_program::ID, false),
        ]
    }

    fn prepare_queue_page_instruction(&self, page_index: u64) -> SolInstruction {
        self.instruction(
            MartingalerInstruction::PrepareQueuePage { page_index },
            self.prepare_queue_page_metas(page_index),
        )
    }

    fn prepare_queue_page_metas(&self, page_index: u64) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.controller.pubkey(), true),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.queue_page(page_index), false),
            AccountMeta::new_readonly(system_program::ID, false),
        ]
    }

    fn profitable_close_instruction(
        &self,
        primary_oracle: Pubkey,
        settlement_shape: ProfitSettlementShape,
    ) -> SolInstruction {
        self.profitable_close_instruction_with_oracles(primary_oracle, None, settlement_shape)
    }

    fn profitable_close_instruction_with_oracles(
        &self,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
        settlement_shape: ProfitSettlementShape,
    ) -> SolInstruction {
        let accounts = self.profitable_close_metas_with_oracles(
            primary_oracle,
            secondary_oracle,
            settlement_shape,
        );
        self.instruction(
            MartingalerInstruction::CloseProfitableCashPosition {
                position_id: POSITION_ID,
                settlement_shape,
            },
            accounts,
        )
    }

    fn profitable_close_metas_with_oracles(
        &self,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
        settlement_shape: ProfitSettlementShape,
    ) -> Vec<AccountMeta> {
        let mut accounts = vec![
            AccountMeta::new(self.controller.pubkey(), true),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.market, false),
            AccountMeta::new(self.controller_risk, false),
            AccountMeta::new(self.position, false),
            AccountMeta::new_readonly(self.oracle_policy, false),
            AccountMeta::new_readonly(self.vault_authority, false),
            AccountMeta::new(self.vault, false),
            AccountMeta::new(self.source, false),
            AccountMeta::new_readonly(self.quote_mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(primary_oracle, false),
        ];
        if let Some(secondary_oracle) = secondary_oracle {
            accounts.push(AccountMeta::new_readonly(secondary_oracle, false));
        }
        if settlement_shape != ProfitSettlementShape::NoClaim {
            accounts.push(AccountMeta::new(self.claim(), false));
            accounts.push(AccountMeta::new(self.queue_page(0), false));
            if settlement_shape == ProfitSettlementShape::Rollover {
                accounts.push(AccountMeta::new(self.queue_page(1), false));
            }
        }
        accounts
    }

    fn controller_ata(&self) -> Pubkey {
        derive_canonical_vault(&self.controller.pubkey(), &self.quote_mint)
    }

    fn service_fifo_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.controller_risk, false),
            AccountMeta::new(self.claim(), false),
            AccountMeta::new(self.page_zero, false),
            AccountMeta::new_readonly(self.vault_authority, false),
            AccountMeta::new(self.vault, false),
            AccountMeta::new(self.controller_ata(), false),
            AccountMeta::new_readonly(self.quote_mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ]
    }

    fn service_fifo_instruction(&self) -> SolInstruction {
        self.service_fifo_instruction_for(self.claim(), self.page_zero)
    }

    fn service_fifo_instruction_for(&self, claim: Pubkey, head_page: Pubkey) -> SolInstruction {
        let mut accounts = self.service_fifo_metas();
        accounts[2] = AccountMeta::new(claim, false);
        accounts[3] = AccountMeta::new(head_page, false);
        self.instruction(MartingalerInstruction::ServiceFifoHead, accounts)
    }

    fn pop_fifo_metas(&self, shape: PopFifoShape) -> Vec<AccountMeta> {
        let mut accounts = vec![
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.claim(), false),
            AccountMeta::new(self.page_zero, false),
        ];
        if shape == PopFifoShape::NextPage {
            accounts.push(AccountMeta::new_readonly(self.queue_page(1), false));
        }
        accounts
    }

    fn pop_fifo_instruction(&self, shape: PopFifoShape) -> SolInstruction {
        self.pop_fifo_instruction_for(self.claim(), self.page_zero, shape)
    }

    fn pop_fifo_instruction_for(
        &self,
        claim: Pubkey,
        head_page: Pubkey,
        shape: PopFifoShape,
    ) -> SolInstruction {
        let mut accounts = self.pop_fifo_metas(shape);
        accounts[1] = AccountMeta::new(claim, false);
        accounts[2] = AccountMeta::new(head_page, false);
        self.instruction(MartingalerInstruction::PopFifoHead { shape }, accounts)
    }

    fn bankruptcy_metas(
        &self,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut accounts = vec![
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.market, false),
            AccountMeta::new(self.controller_risk, false),
            AccountMeta::new(self.position, false),
            AccountMeta::new_readonly(self.oracle_policy, false),
            AccountMeta::new_readonly(self.vault, false),
            AccountMeta::new_readonly(primary_oracle, false),
        ];
        if let Some(secondary_oracle) = secondary_oracle {
            accounts.push(AccountMeta::new_readonly(secondary_oracle, false));
        }
        accounts
    }

    fn bankruptcy_instruction(
        &self,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
    ) -> SolInstruction {
        self.instruction(
            MartingalerInstruction::ResolveBankruptCashPosition {
                position_id: POSITION_ID,
            },
            self.bankruptcy_metas(primary_oracle, secondary_oracle),
        )
    }

    fn open_claim_instruction(
        &self,
        position_id: u64,
        source_claim: Pubkey,
        source_page: Pubkey,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
        claim_amount: u64,
        notional: u64,
        leverage_x: u16,
    ) -> SolInstruction {
        let mut accounts = vec![
            AccountMeta::new(self.controller.pubkey(), true),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.market, false),
            AccountMeta::new(self.controller_risk, false),
            AccountMeta::new(source_claim, false),
            AccountMeta::new(source_page, false),
            AccountMeta::new(self.position_for(position_id), false),
            AccountMeta::new_readonly(self.oracle_policy, false),
            AccountMeta::new_readonly(self.vault, false),
            AccountMeta::new_readonly(primary_oracle, false),
        ];
        if let Some(secondary_oracle) = secondary_oracle {
            accounts.push(AccountMeta::new_readonly(secondary_oracle, false));
        }
        self.instruction(
            MartingalerInstruction::OpenClaimPosition {
                position_id,
                market_id: MARKET_ID,
                side: PositionSide::Long,
                claim_amount,
                notional,
                leverage_x,
            },
            accounts,
        )
    }

    fn close_claim_instruction(
        &self,
        position_id: u64,
        source_claim: Pubkey,
        primary_oracle: Pubkey,
        settlement_shape: ProfitSettlementShape,
        profitable: bool,
    ) -> SolInstruction {
        self.close_claim_instruction_with_oracles(
            position_id,
            source_claim,
            primary_oracle,
            None,
            settlement_shape,
            profitable,
        )
    }

    fn close_claim_instruction_with_oracles(
        &self,
        position_id: u64,
        source_claim: Pubkey,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
        settlement_shape: ProfitSettlementShape,
        profitable: bool,
    ) -> SolInstruction {
        let position = self.position_for(position_id);
        let mut accounts = vec![
            AccountMeta::new(self.controller.pubkey(), true),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.market, false),
            AccountMeta::new(self.controller_risk, false),
            AccountMeta::new(position, false),
            AccountMeta::new(source_claim, false),
            AccountMeta::new_readonly(self.oracle_policy, false),
            AccountMeta::new_readonly(self.vault_authority, false),
            AccountMeta::new(self.vault, false),
            AccountMeta::new(self.source, false),
            AccountMeta::new_readonly(self.quote_mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(primary_oracle, false),
        ];
        if let Some(secondary_oracle) = secondary_oracle {
            accounts.push(AccountMeta::new_readonly(secondary_oracle, false));
        }
        if settlement_shape != ProfitSettlementShape::NoClaim {
            accounts.push(AccountMeta::new(self.claim_for(position_id), false));
            accounts.push(AccountMeta::new(self.page_zero, false));
            if settlement_shape == ProfitSettlementShape::Rollover {
                accounts.push(AccountMeta::new(self.queue_page(1), false));
            }
        }
        let instruction = if profitable {
            MartingalerInstruction::CloseProfitableClaimPosition {
                position_id,
                settlement_shape,
            }
        } else {
            MartingalerInstruction::CloseNonPositiveClaimPosition {
                position_id,
                settlement_shape,
            }
        };
        self.instruction(instruction, accounts)
    }

    fn claim_bankruptcy_instruction(
        &self,
        position_id: u64,
        source_claim: Pubkey,
        primary_oracle: Pubkey,
        secondary_oracle: Option<Pubkey>,
    ) -> SolInstruction {
        let mut accounts = vec![
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.market, false),
            AccountMeta::new(self.controller_risk, false),
            AccountMeta::new(self.position_for(position_id), false),
            AccountMeta::new(source_claim, false),
            AccountMeta::new_readonly(self.oracle_policy, false),
            AccountMeta::new_readonly(self.vault, false),
            AccountMeta::new_readonly(primary_oracle, false),
        ];
        if let Some(secondary_oracle) = secondary_oracle {
            accounts.push(AccountMeta::new_readonly(secondary_oracle, false));
        }
        self.instruction(
            MartingalerInstruction::ResolveBankruptClaimPosition { position_id },
            accounts,
        )
    }

    fn set_clock(&mut self, unix_timestamp: i64) {
        let mut clock = self.svm.get_sysvar::<Clock>();
        clock.unix_timestamp = unix_timestamp;
        self.svm.set_sysvar(&clock);
    }

    fn install_pyth_price(
        &mut self,
        feed_id: &[u8; 32],
        price: i64,
        conf: u64,
        publish_time: i64,
    ) -> Pubkey {
        let key = Pubkey::new_unique();
        self.svm
            .set_account(
                key,
                Account {
                    lamports: 1_000_000_000,
                    data: make_pyth_data(feed_id, price, -6, conf, publish_time),
                    owner: oracle_v16::PYTH_RECEIVER_PROGRAM_ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .expect("install Pyth PriceUpdateV2 fixture");
        key
    }

    fn install_switchboard_price(
        &mut self,
        feed_id: &[u8; 32],
        price_e6: u64,
        std_dev_e6: u64,
        publish_time: i64,
    ) -> Pubkey {
        self.install_switchboard_price_with_owner(
            feed_id,
            price_e6,
            std_dev_e6,
            publish_time,
            oracle_v16::SWITCHBOARD_ON_DEMAND_DEVNET_PROGRAM_ID,
        )
    }

    fn install_switchboard_price_with_owner(
        &mut self,
        feed_id: &[u8; 32],
        price_e6: u64,
        std_dev_e6: u64,
        publish_time: i64,
        owner: Pubkey,
    ) -> Pubkey {
        let key = Pubkey::new_from_array(*feed_id);
        let value = i128::from(price_e6) * 1_000_000_000_000;
        let std_dev = i128::from(std_dev_e6) * 1_000_000_000_000;
        self.svm
            .set_account(
                key,
                Account {
                    lamports: 1_000_000_000,
                    data: make_switchboard_data(feed_id, value, std_dev, publish_time, 1, 1, 1),
                    owner,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .expect("install Switchboard On-Demand PullFeed fixture");
        key
    }

    fn send(
        &mut self,
        instructions: Vec<SolInstruction>,
        admin_signs: bool,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        let mut transaction_instructions = vec![
            ComputeBudgetInstruction::request_heap_frame(128 * 1024),
            ComputeBudgetInstruction::set_compute_unit_limit(TX_CU_LIMIT),
        ];
        transaction_instructions.extend(instructions);
        let tx = if admin_signs {
            Transaction::new_signed_with_payer(
                &transaction_instructions,
                Some(&self.payer.pubkey()),
                &[&self.payer, &self.admin],
                self.svm.latest_blockhash(),
            )
        } else {
            Transaction::new_signed_with_payer(
                &transaction_instructions,
                Some(&self.payer.pubkey()),
                &[&self.payer],
                self.svm.latest_blockhash(),
            )
        };
        self.svm.send_transaction(tx).map_err(Box::new)
    }

    fn send_bundle(
        &mut self,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        self.send(vec![self.prepare(), self.initialize()], true)
    }

    fn send_controller(
        &mut self,
        instructions: Vec<SolInstruction>,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        let mut transaction_instructions = vec![
            ComputeBudgetInstruction::request_heap_frame(128 * 1024),
            ComputeBudgetInstruction::set_compute_unit_limit(TX_CU_LIMIT),
        ];
        transaction_instructions.extend(instructions);
        let tx = Transaction::new_signed_with_payer(
            &transaction_instructions,
            Some(&self.payer.pubkey()),
            &[&self.payer, &self.controller],
            self.svm.latest_blockhash(),
        );
        self.svm.send_transaction(tx).map_err(Box::new)
    }
}

fn deterministic_keypair(seed: u8, label: u8) -> Keypair {
    let mut bytes = [0u8; 32];
    bytes[0] = seed;
    bytes[1] = label;
    bytes[31] = seed ^ label ^ 0xA5;
    keypair_from_seed(&bytes).expect("deterministic fixture keypair")
}

fn program_path() -> PathBuf {
    if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(target_dir)
            .join("deploy")
            .join("percolator_prog.so");
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/deploy/percolator_prog.so");
    assert!(path.exists(), "build exact program SBF before this test");
    path
}

fn spl_token_program_path() -> PathBuf {
    bundled_litesvm_program("spl_token-3.5.0.so")
}

fn associated_token_program_path() -> PathBuf {
    bundled_litesvm_program("spl_associated_token_account-1.1.1.so")
}

fn bundled_litesvm_program(filename: &str) -> PathBuf {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".cargo"));
    let registry_src = cargo_home.join("registry/src");
    for registry in std::fs::read_dir(&registry_src).expect("registry/src") {
        let candidate = registry
            .expect("registry entry")
            .path()
            .join("litesvm-0.1.0/src/spl/programs")
            .join(filename);
        if candidate.exists() {
            return candidate;
        }
    }
    panic!("could not locate {filename} under {registry_src:?}");
}

fn associated_token_program_id() -> Pubkey {
    solana_sdk::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")
}

fn make_pyth_data(
    feed_id: &[u8; 32],
    price: i64,
    exponent: i32,
    confidence: u64,
    publish_time: i64,
) -> Vec<u8> {
    let mut data = vec![0u8; 134];
    data[0..8].copy_from_slice(&[0x22, 0xf1, 0x23, 0x63, 0x9d, 0x7e, 0xf4, 0xcd]);
    data[40] = 1;
    data[41..73].copy_from_slice(feed_id);
    data[73..81].copy_from_slice(&price.to_le_bytes());
    data[81..89].copy_from_slice(&confidence.to_le_bytes());
    data[89..93].copy_from_slice(&exponent.to_le_bytes());
    data[93..101].copy_from_slice(&publish_time.to_le_bytes());
    data
}

fn make_switchboard_data(
    feed_hash: &[u8; 32],
    value: i128,
    std_dev: i128,
    publish_time: i64,
    num_samples: u8,
    min_sample_size: u8,
    result_slot: u64,
) -> Vec<u8> {
    let mut data = vec![0u8; 3_208];
    data[0..8].copy_from_slice(&[196, 27, 108, 196, 10, 215, 219, 40]);
    data[2120..2152].copy_from_slice(feed_hash);
    data[2215] = min_sample_size;
    data[2216..2224].copy_from_slice(&publish_time.to_le_bytes());
    data[2264..2280].copy_from_slice(&value.to_le_bytes());
    data[2280..2296].copy_from_slice(&std_dev.to_le_bytes());
    data[2360] = num_samples;
    data[2361] = 0;
    data[2368..2376].copy_from_slice(&result_slot.to_le_bytes());
    data[2952..2960].copy_from_slice(&publish_time.to_le_bytes());
    data
}

fn make_chainlink_data(decimals: u8, result_slot: u64, publish_time: u32, answer: i128) -> Vec<u8> {
    let mut data = vec![0u8; 8 + 192 + 48];
    data[0..8].copy_from_slice(&[96, 179, 69, 66, 128, 129, 73, 117]);
    data[8] = 1;
    data[138] = decimals;
    data[143..147].copy_from_slice(&1u32.to_le_bytes());
    data[148..152].copy_from_slice(&1u32.to_le_bytes());
    data[200..208].copy_from_slice(&result_slot.to_le_bytes());
    data[208..212].copy_from_slice(&publish_time.to_le_bytes());
    data[216..232].copy_from_slice(&answer.to_le_bytes());
    data
}

fn custody_snapshot(fixture: &Fixture) -> (Account, Account, Account, Account, Account) {
    (
        fixture.svm.get_account(&fixture.pool).unwrap(),
        fixture.svm.get_account(&fixture.controller_risk).unwrap(),
        fixture.svm.get_account(&fixture.source).unwrap(),
        fixture.svm.get_account(&fixture.vault).unwrap(),
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
    )
}

fn cash_open_snapshot(fixture: &Fixture, oracle_keys: &[Pubkey]) -> Vec<Option<Account>> {
    let mut keys = vec![
        fixture.controller.pubkey(),
        fixture.pool,
        fixture.market,
        fixture.controller_risk,
        fixture.position,
        fixture.oracle_policy,
        fixture.vault,
        fixture.source,
        fixture.quote_mint,
    ];
    keys.extend_from_slice(oracle_keys);
    keys.iter()
        .map(|key| fixture.svm.get_account(key))
        .collect()
}

fn account_snapshot(fixture: &Fixture, keys: &[Pubkey]) -> Vec<Option<Account>> {
    keys.iter()
        .map(|key| fixture.svm.get_account(key))
        .collect()
}

fn opened_profitable_fixture(seed: u8, entry_price_e6: i64) -> Fixture {
    let mut fixture = Fixture::new(seed);
    bootstrap_cash_open(&mut fixture, false);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, entry_price_e6, 1, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("open profitable-close boundary fixture");
    fixture
}

fn install_controller_ata(fixture: &mut Fixture) {
    let mut data = vec![0u8; TokenAccount::LEN];
    TokenAccount::pack(
        TokenAccount {
            mint: fixture.quote_mint,
            owner: fixture.controller.pubkey(),
            amount: 0,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        },
        &mut data,
    )
    .unwrap();
    fixture
        .svm
        .set_account(
            fixture.controller_ata(),
            Account {
                lamports: Rent::default().minimum_balance(TokenAccount::LEN),
                data,
                owner: spl_token::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

fn queued_claim_fixture(seed: u8, service_cash: u64, status: PoolStatus) -> Fixture {
    let mut fixture = opened_profitable_fixture(seed, 100_000_000);
    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_claim_instruction(),
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::CurrentTail),
        ])
        .expect("create canonical queued profit claim");
    install_controller_ata(&mut fixture);

    let mut pool_account = fixture.svm.get_account(&fixture.pool).unwrap();
    let mut pool_state = decode_account::<PoolState>(&pool_account.data).unwrap();
    assert_eq!(pool_state.total_queued.get(), u128::from(PROFIT_CLAIM_FACE));
    pool_state.status = status as u8;
    pool_state.idle_cash_collateral = U128Le::new(
        pool_state
            .idle_cash_collateral
            .get()
            .checked_sub(u128::from(service_cash))
            .unwrap(),
    );
    pool_state.claim_service_cash = U128Le::new(u128::from(service_cash));
    overwrite_account(&mut pool_account.data, &pool_state).unwrap();
    fixture.svm.set_account(fixture.pool, pool_account).unwrap();
    fixture
}

const CLAIM_POSITION_ID: u64 = POSITION_ID + 1;

fn opened_claim_position_fixture(seed: u8) -> Fixture {
    let mut fixture = queued_claim_fixture(seed, 0, PoolStatus::Active);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction_for(CLAIM_POSITION_ID),
            fixture.open_claim_instruction(
                CLAIM_POSITION_ID,
                fixture.claim(),
                fixture.page_zero,
                entry,
                None,
                PROFIT_CLAIM_FACE,
                882_000_000,
                100,
            ),
        ])
        .expect("open claim-funded position from canonical FIFO claim");
    fixture
}

fn opened_claim_position_dual_fixture(seed: u8) -> Fixture {
    let mut fixture = Fixture::new(seed);
    bootstrap_cash_open(&mut fixture, true);
    let cash_entry =
        fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let cash_entry_secondary =
        fixture.install_switchboard_price(&SECONDARY_FEED_ID, 100_200_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(
                cash_entry,
                Some(cash_entry_secondary),
                OPEN_COLLATERAL,
                OPEN_NOTIONAL,
                100,
            ),
        ])
        .expect("open dual-oracle cash position for claim funding");
    let cash_exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    let cash_exit_secondary =
        fixture.install_switchboard_price(&SECONDARY_FEED_ID, 101_200_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_claim_instruction(),
            fixture.profitable_close_instruction_with_oracles(
                cash_exit,
                Some(cash_exit_secondary),
                ProfitSettlementShape::CurrentTail,
            ),
        ])
        .expect("create dual-oracle queued claim");
    install_controller_ata(&mut fixture);

    let claim_entry =
        fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let claim_entry_secondary =
        fixture.install_switchboard_price(&SECONDARY_FEED_ID, 100_200_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction_for(CLAIM_POSITION_ID),
            fixture.open_claim_instruction(
                CLAIM_POSITION_ID,
                fixture.claim(),
                fixture.page_zero,
                claim_entry,
                Some(claim_entry_secondary),
                PROFIT_CLAIM_FACE,
                882_000_000,
                100,
            ),
        ])
        .expect("open dual-oracle claim-funded position");
    fixture
}

fn fully_serviced_claim_fixture(seed: u8) -> Fixture {
    let mut fixture = queued_claim_fixture(seed, PROFIT_CLAIM_FACE, PoolStatus::CloseOnly);
    fixture
        .send(vec![fixture.service_fifo_instruction()], false)
        .expect("fully service canonical FIFO head");
    fixture
}

fn cross_page_queued_claim_fixture(seed: u8) -> Fixture {
    let mut fixture = queued_claim_fixture(seed, PROFIT_CLAIM_FACE, PoolStatus::CloseOnly);
    let claim_key = fixture.claim();

    let mut claim_account = fixture.svm.get_account(&claim_key).unwrap();
    let mut claim = decode_account::<ClaimState>(&claim_account.data).unwrap();
    claim.sequence = U128Le::new(32);
    claim.queue_slot = U16Le::new(31);
    overwrite_account(&mut claim_account.data, &claim).unwrap();
    fixture.svm.set_account(claim_key, claim_account).unwrap();

    let mut first_account = fixture.svm.get_account(&fixture.page_zero).unwrap();
    let mut first = decode_account::<QueuePageState>(&first_account.data).unwrap();
    first.entries = [QueueEntry::default(); QUEUE_PAGE_CAPACITY];
    first.head_offset = U16Le::new(31);
    first.len = U16Le::new(32);
    first.next_append_sequence = U128Le::new(33);
    first.next_page_index = percolator_prog::martingaler_state::U64Le::new(1);
    first.entries[31] = QueueEntry {
        claim: Key32::new(claim_key.to_bytes()),
        sequence: U128Le::new(32),
        remaining_face: U128Le::new(u128::from(PROFIT_CLAIM_FACE)),
    };
    overwrite_account(&mut first_account.data, &first).unwrap();
    fixture
        .svm
        .set_account(fixture.page_zero, first_account)
        .unwrap();

    let mut next = QueuePageState::new(Key32::new(fixture.pool.to_bytes()), 1, 0, 33).unwrap();
    next.entries[0] = QueueEntry {
        claim: Key32::new([0xCC; 32]),
        sequence: U128Le::new(33),
        remaining_face: U128Le::new(1),
    };
    next.len = U16Le::new(1);
    next.next_append_sequence = U128Le::new(34);
    let (_, next_bump) = derive_queue_page(&id(), &fixture.pool, 1);
    let mut next_data = vec![0; account_len::<QueuePageState>()];
    initialize_account(&mut next_data, next_bump, &next).unwrap();
    fixture
        .svm
        .set_account(
            fixture.queue_page(1),
            Account {
                lamports: Rent::default().minimum_balance(next_data.len()),
                data: next_data,
                owner: id(),
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();

    let mut pool_account = fixture.svm.get_account(&fixture.pool).unwrap();
    let mut pool = decode_account::<PoolState>(&pool_account.data).unwrap();
    pool.total_queued = U128Le::new(u128::from(PROFIT_CLAIM_FACE) + 1);
    pool.next_claim_sequence = U128Le::new(34);
    pool.head_claim_sequence = U128Le::new(32);
    pool.tail_claim_sequence = U128Le::new(33);
    pool.head_page_index = percolator_prog::martingaler_state::U64Le::new(0);
    pool.tail_page_index = percolator_prog::martingaler_state::U64Le::new(1);
    pool.next_page_index = percolator_prog::martingaler_state::U64Le::new(2);
    pool.queue_flags = 0;
    overwrite_account(&mut pool_account.data, &pool).unwrap();
    fixture.svm.set_account(fixture.pool, pool_account).unwrap();
    fixture
}

fn fully_serviced_cross_page_claim_fixture(seed: u8) -> Fixture {
    let mut fixture = cross_page_queued_claim_fixture(seed);
    fixture
        .send(vec![fixture.service_fifo_instruction()], false)
        .expect("fully service canonical cross-page FIFO head");
    fixture
}

fn fill_current_tail(fixture: &mut Fixture) {
    let mut pool_account = fixture.svm.get_account(&fixture.pool).unwrap();
    let mut pool_state = decode_account::<PoolState>(&pool_account.data).unwrap();
    pool_state.total_queued = U128Le::new(QUEUE_PAGE_CAPACITY as u128);
    pool_state.next_claim_sequence = U128Le::new(QUEUE_PAGE_CAPACITY as u128 + 1);
    pool_state.head_claim_sequence = U128Le::new(1);
    pool_state.tail_claim_sequence = U128Le::new(QUEUE_PAGE_CAPACITY as u128);
    overwrite_account(&mut pool_account.data, &pool_state).unwrap();
    fixture.svm.set_account(fixture.pool, pool_account).unwrap();

    let mut page_account = fixture.svm.get_account(&fixture.page_zero).unwrap();
    let mut page_state = decode_account::<QueuePageState>(&page_account.data).unwrap();
    for (slot, entry) in page_state.entries.iter_mut().enumerate() {
        *entry = QueueEntry {
            claim: Key32::new([slot as u8 + 1; 32]),
            sequence: U128Le::new(slot as u128 + 1),
            remaining_face: U128Le::new(1),
        };
    }
    page_state.len = U16Le::new(QUEUE_PAGE_CAPACITY as u16);
    page_state.next_append_sequence = U128Le::new(QUEUE_PAGE_CAPACITY as u128 + 1);
    overwrite_account(&mut page_account.data, &page_state).unwrap();
    fixture
        .svm
        .set_account(fixture.page_zero, page_account)
        .unwrap();
}

fn replace_account(fixture: &mut Fixture, key: Pubkey, owner: Pubkey, len: usize, rent: bool) {
    fixture
        .svm
        .set_account(
            key,
            Account {
                lamports: if rent {
                    Rent::default().minimum_balance(len)
                } else {
                    1
                },
                data: vec![0; len],
                owner,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

fn close_boundary_snapshot(fixture: &Fixture, oracle: Pubkey) -> Vec<Option<Account>> {
    account_snapshot(
        fixture,
        &[
            fixture.controller.pubkey(),
            fixture.pool,
            fixture.market,
            fixture.controller_risk,
            fixture.position,
            fixture.oracle_policy,
            fixture.vault,
            fixture.source,
            fixture.quote_mint,
            fixture.claim(),
            fixture.page_zero,
            fixture.queue_page(1),
            oracle,
        ],
    )
}

fn claim_service_snapshot(fixture: &Fixture) -> Vec<Option<Account>> {
    account_snapshot(
        fixture,
        &[
            fixture.pool,
            fixture.controller_risk,
            fixture.claim(),
            fixture.page_zero,
            fixture.vault,
            fixture.controller_ata(),
            fixture.quote_mint,
            fixture.queue_page(1),
        ],
    )
}

fn bootstrap_cash_open(fixture: &mut Fixture, dual_oracle: bool) {
    fixture.send_bundle().expect("initialize canonical pool");
    fixture
        .send(
            vec![
                fixture.market_instruction(true),
                fixture.market_instruction(false),
            ],
            true,
        )
        .expect("register canonical market");
    fixture
        .send_controller(vec![
            fixture.controller_instruction(true),
            fixture.controller_instruction(false),
        ])
        .expect("register canonical controller");
    fixture
        .send(vec![fixture.vault_instruction()], true)
        .expect("initialize canonical vault");
    fixture
        .send_controller(vec![fixture.deposit_instruction(DEPOSIT_AMOUNT)])
        .expect("deposit controller cash");
    fixture
        .send(
            vec![
                fixture.oracle_policy_instruction(true, dual_oracle),
                fixture.oracle_policy_instruction(false, dual_oracle),
            ],
            true,
        )
        .expect("register immutable oracle policy");
    fixture.set_clock(ORACLE_TIME);
}

#[test]
fn instruction_codec_is_strict_and_legacy_namespace_is_unchanged() {
    for (instruction, expected_len) in [
        (
            MartingalerInstruction::PreparePool {
                risk_pool_id: RISK_POOL_ID,
            },
            MARTINGALER_IX_LEN,
        ),
        (
            MartingalerInstruction::InitializePool {
                risk_pool_id: RISK_POOL_ID,
            },
            MARTINGALER_IX_LEN,
        ),
        (
            MartingalerInstruction::PrepareMarket {
                market_id: MARKET_ID,
            },
            PREPARE_MARKET_IX_LEN,
        ),
        (
            MartingalerInstruction::InitializeMarket {
                market_id: MARKET_ID,
                divergence_limit_bps: DIVERGENCE_LIMIT_BPS,
                max_long_open_interest: V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
                max_short_open_interest: V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
                haircut: haircut(),
            },
            INITIALIZE_MARKET_IX_LEN,
        ),
        (MartingalerInstruction::PrepareController, CONTROLLER_IX_LEN),
        (
            MartingalerInstruction::InitializeController,
            CONTROLLER_IX_LEN,
        ),
        (
            MartingalerInstruction::InitializeVault,
            INITIALIZE_VAULT_IX_LEN,
        ),
        (
            MartingalerInstruction::DepositCash {
                amount: DEPOSIT_AMOUNT,
            },
            DEPOSIT_CASH_IX_LEN,
        ),
        (
            MartingalerInstruction::PrepareOraclePolicy {
                market_id: MARKET_ID,
            },
            PREPARE_ORACLE_POLICY_IX_LEN,
        ),
        (
            MartingalerInstruction::InitializeOraclePolicy {
                market_id: MARKET_ID,
                primary_feed_id: PRIMARY_FEED_ID,
                secondary_feed_id: SECONDARY_FEED_ID,
                max_staleness_secs: 30,
                confidence_limit_bps: 100,
            },
            INITIALIZE_ORACLE_POLICY_IX_LEN,
        ),
        (
            MartingalerInstruction::PrepareClaim {
                position_id: POSITION_ID,
            },
            PREPARE_CLAIM_IX_LEN,
        ),
        (
            MartingalerInstruction::PrepareQueuePage { page_index: 1 },
            PREPARE_QUEUE_PAGE_IX_LEN,
        ),
        (
            MartingalerInstruction::CloseProfitableCashPosition {
                position_id: POSITION_ID,
                settlement_shape: ProfitSettlementShape::Rollover,
            },
            CLOSE_PROFITABLE_CASH_POSITION_IX_LEN,
        ),
        (
            MartingalerInstruction::ServiceFifoHead,
            SERVICE_FIFO_HEAD_IX_LEN,
        ),
        (
            MartingalerInstruction::PopFifoHead {
                shape: PopFifoShape::NextPage,
            },
            POP_FIFO_HEAD_IX_LEN,
        ),
        (
            MartingalerInstruction::PreparePosition {
                position_id: POSITION_ID,
            },
            PREPARE_POSITION_IX_LEN,
        ),
        (
            MartingalerInstruction::OpenCashPosition {
                position_id: POSITION_ID,
                market_id: MARKET_ID,
                side: PositionSide::Long,
                collateral: OPEN_COLLATERAL,
                notional: OPEN_NOTIONAL,
                leverage_x: 100,
            },
            OPEN_CASH_POSITION_IX_LEN,
        ),
        (
            MartingalerInstruction::CloseNonPositiveCashPosition {
                position_id: POSITION_ID,
            },
            CLOSE_CASH_POSITION_IX_LEN,
        ),
        (
            MartingalerInstruction::ResolveBankruptCashPosition {
                position_id: POSITION_ID,
            },
            CLOSE_CASH_POSITION_IX_LEN,
        ),
        (
            MartingalerInstruction::OpenClaimPosition {
                position_id: POSITION_ID + 1,
                market_id: MARKET_ID,
                side: PositionSide::Long,
                claim_amount: PROFIT_CLAIM_FACE,
                notional: OPEN_NOTIONAL,
                leverage_x: 100,
            },
            OPEN_CLAIM_POSITION_IX_LEN,
        ),
        (
            MartingalerInstruction::CloseNonPositiveClaimPosition {
                position_id: POSITION_ID + 1,
                settlement_shape: ProfitSettlementShape::CurrentTail,
            },
            CLOSE_CLAIM_POSITION_IX_LEN,
        ),
        (
            MartingalerInstruction::ResolveBankruptClaimPosition {
                position_id: POSITION_ID + 1,
            },
            RESOLVE_BANKRUPT_CLAIM_POSITION_IX_LEN,
        ),
        (
            MartingalerInstruction::CloseProfitableClaimPosition {
                position_id: POSITION_ID + 1,
                settlement_shape: ProfitSettlementShape::Rollover,
            },
            CLOSE_CLAIM_POSITION_IX_LEN,
        ),
        (
            MartingalerInstruction::SetPoolStatus {
                target: PoolStatus::CloseOnly,
            },
            SET_STATUS_IX_LEN,
        ),
        (
            MartingalerInstruction::SetMarketStatus {
                target: PoolStatus::Terminal,
            },
            SET_STATUS_IX_LEN,
        ),
        (
            MartingalerInstruction::WithdrawIdleCash { amount: 1 },
            WITHDRAW_IDLE_CASH_IX_LEN,
        ),
    ] {
        let encoded = instruction.encode();
        assert_eq!(encoded.len(), expected_len);
        assert_eq!(&encoded[..8], &MARTINGALER_IX_MAGIC);
        assert_eq!(
            u16::from_le_bytes(encoded[8..10].try_into().unwrap()),
            MARTINGALER_IX_VERSION
        );
        assert_eq!(
            MartingalerInstruction::decode(&encoded).unwrap(),
            instruction
        );
        assert!(has_namespace(&encoded));
    }

    let valid = MartingalerInstruction::PreparePool {
        risk_pool_id: RISK_POOL_ID,
    }
    .encode();
    for malformed in [
        valid[..18].to_vec(),
        [valid.as_slice(), &[0]].concat(),
        {
            let mut data = valid.clone();
            data[0] ^= 1;
            data
        },
        {
            let mut data = valid.clone();
            data[8..10].copy_from_slice(&(MARTINGALER_IX_VERSION + 1).to_le_bytes());
            data
        },
        {
            let mut data = valid.clone();
            data[10] = 99;
            data
        },
        {
            let mut data = valid.clone();
            data[11..19].fill(0);
            data
        },
    ] {
        assert!(MartingalerInstruction::decode(&malformed).is_err());
    }

    for instruction in [
        MartingalerInstruction::CloseNonPositiveCashPosition {
            position_id: POSITION_ID,
        },
        MartingalerInstruction::ResolveBankruptCashPosition {
            position_id: POSITION_ID,
        },
        MartingalerInstruction::ResolveBankruptClaimPosition {
            position_id: POSITION_ID,
        },
    ] {
        let valid = instruction.encode();
        assert_eq!(valid.len(), 19);
        assert!(MartingalerInstruction::decode(&valid[..18]).is_err());
        assert!(MartingalerInstruction::decode(&[valid.as_slice(), &[0]].concat()).is_err());
        let mut zero_id = valid;
        zero_id[11..19].fill(0);
        assert!(MartingalerInstruction::decode(&zero_id).is_err());
    }

    let pop = MartingalerInstruction::PopFifoHead {
        shape: PopFifoShape::CurrentPageOrEmpty,
    }
    .encode();
    assert!(MartingalerInstruction::decode(&pop[..11]).is_err());
    assert!(MartingalerInstruction::decode(&[pop.as_slice(), &[0]].concat()).is_err());
    let mut unknown_shape = pop;
    unknown_shape[11] = 2;
    assert!(MartingalerInstruction::decode(&unknown_shape).is_err());

    for instruction in [
        MartingalerInstruction::PrepareClaim {
            position_id: POSITION_ID,
        },
        MartingalerInstruction::PrepareQueuePage { page_index: 1 },
        MartingalerInstruction::CloseProfitableCashPosition {
            position_id: POSITION_ID,
            settlement_shape: ProfitSettlementShape::CurrentTail,
        },
        MartingalerInstruction::CloseNonPositiveClaimPosition {
            position_id: POSITION_ID,
            settlement_shape: ProfitSettlementShape::CurrentTail,
        },
        MartingalerInstruction::CloseProfitableClaimPosition {
            position_id: POSITION_ID,
            settlement_shape: ProfitSettlementShape::CurrentTail,
        },
    ] {
        let valid = instruction.encode();
        assert!(MartingalerInstruction::decode(&valid[..valid.len() - 1]).is_err());
        assert!(MartingalerInstruction::decode(&[valid.as_slice(), &[0]].concat()).is_err());
    }
    let mut invalid_shape = MartingalerInstruction::CloseProfitableCashPosition {
        position_id: POSITION_ID,
        settlement_shape: ProfitSettlementShape::NoClaim,
    }
    .encode();
    *invalid_shape.last_mut().unwrap() = 3;
    assert!(MartingalerInstruction::decode(&invalid_shape).is_err());

    let mut invalid_status = MartingalerInstruction::SetPoolStatus {
        target: PoolStatus::CloseOnly,
    }
    .encode();
    *invalid_status.last_mut().unwrap() = 4;
    assert!(MartingalerInstruction::decode(&invalid_status).is_err());
    let zero_withdraw = {
        let mut data = MartingalerInstruction::WithdrawIdleCash { amount: 1 }.encode();
        data[11..19].fill(0);
        data
    };
    assert!(MartingalerInstruction::decode(&zero_withdraw).is_err());

    let old_initialize_market = MartingalerInstruction::InitializeMarket {
        market_id: MARKET_ID,
        divergence_limit_bps: DIVERGENCE_LIMIT_BPS,
        max_long_open_interest: V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
        max_short_open_interest: V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
        haircut: haircut(),
    }
    .encode()[..47]
        .to_vec();
    assert!(MartingalerInstruction::decode(&old_initialize_market).is_err());

    let mut prepare_market_with_trailing_risk_fields = MartingalerInstruction::PrepareMarket {
        market_id: MARKET_ID,
    }
    .encode();
    prepare_market_with_trailing_risk_fields.extend_from_slice(&[0u8; 34]);
    assert_eq!(prepare_market_with_trailing_risk_fields.len(), 47);
    assert!(MartingalerInstruction::decode(&prepare_market_with_trailing_risk_fields).is_err());

    for malformed in [
        MartingalerInstruction::DepositCash { amount: 1 }.encode()[..18].to_vec(),
        [
            MartingalerInstruction::DepositCash { amount: 1 }
                .encode()
                .as_slice(),
            &[0],
        ]
        .concat(),
        MartingalerInstruction::DepositCash { amount: 0 }
            .encode()
            .to_vec(),
    ] {
        assert!(MartingalerInstruction::decode(&malformed).is_err());
    }

    let legacy = V16Instruction::InitPortfolio.encode();
    assert!(!has_namespace(&legacy));
    assert!(V16Instruction::decode(&legacy).is_ok());
    assert_eq!(
        dispatch_instruction(&id(), &[], &legacy),
        processor::process_instruction(&id(), &[], &legacy)
    );
    let formerly_unknown_tag = [70u8];
    assert!(!has_namespace(&formerly_unknown_tag));
    assert_eq!(
        dispatch_instruction(&id(), &[], &formerly_unknown_tag),
        processor::process_instruction(&id(), &[], &formerly_unknown_tag)
    );
}

#[test]
fn real_sbf_uses_the_runtime_program_id_for_a_fresh_isolated_deployment() {
    let runtime_program_id = Pubkey::new_from_array([0x6d; 32]);
    assert_ne!(runtime_program_id, id());
    let mut svm = LiteSVM::new();
    svm.add_program(
        runtime_program_id,
        &std::fs::read(program_path()).expect("read exact Martingaler program SBF"),
    );
    let admin = deterministic_keypair(201, 1);
    let quote_mint = deterministic_keypair(201, 2).pubkey();
    svm.airdrop(&admin.pubkey(), 10_000_000_000)
        .expect("fund alternate-program admin");
    let mut mint_data = vec![0u8; Mint::LEN];
    Mint::pack(
        Mint {
            mint_authority: COption::None,
            supply: 0,
            decimals: V1_QUOTE_DECIMALS,
            is_initialized: true,
            freeze_authority: COption::None,
        },
        &mut mint_data,
    )
    .expect("pack alternate-program quote mint");
    svm.set_account(
        quote_mint,
        Account {
            lamports: Rent::default().minimum_balance(Mint::LEN),
            data: mint_data,
            owner: spl_token::ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("install alternate-program quote mint");
    let (pool, _) = derive_pool(
        &runtime_program_id,
        &admin.pubkey(),
        &quote_mint,
        RISK_POOL_ID,
    );
    let (page_zero, _) = derive_queue_page_zero(&runtime_program_id, &pool);
    let metas = vec![
        AccountMeta::new(admin.pubkey(), true),
        AccountMeta::new_readonly(quote_mint, false),
        AccountMeta::new(pool, false),
        AccountMeta::new(page_zero, false),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let instruction = |value: MartingalerInstruction| SolInstruction {
        program_id: runtime_program_id,
        accounts: metas.clone(),
        data: value.encode().to_vec(),
    };
    let transaction = Transaction::new_signed_with_payer(
        &[
            instruction(MartingalerInstruction::PreparePool {
                risk_pool_id: RISK_POOL_ID,
            }),
            instruction(MartingalerInstruction::InitializePool {
                risk_pool_id: RISK_POOL_ID,
            }),
        ],
        Some(&admin.pubkey()),
        &[&admin],
        svm.latest_blockhash(),
    );
    svm.send_transaction(transaction)
        .expect("initialize under alternate runtime program id");
    assert_eq!(svm.get_account(&pool).unwrap().owner, runtime_program_id);
    assert_eq!(
        svm.get_account(&page_zero).unwrap().owner,
        runtime_program_id
    );
}

#[test]
fn atomic_bundle_creates_exact_pool_root_and_replay_fails() {
    let mut fixture = Fixture::new(10);
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();
    let result = fixture.send_bundle().expect("initialize pool bundle");
    eprintln!(
        "martingaler_phase2b1_bundle_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed > 0);
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let pool_account = fixture.svm.get_account(&fixture.pool).unwrap();
    let page_account = fixture.svm.get_account(&fixture.page_zero).unwrap();
    assert_eq!(pool_account.owner, id());
    assert_eq!(page_account.owner, id());
    assert_eq!(pool_account.data.len(), account_len::<PoolState>());
    assert_eq!(page_account.data.len(), account_len::<QueuePageState>());
    assert!(Rent::default().is_exempt(pool_account.lamports, pool_account.data.len()));
    assert!(Rent::default().is_exempt(page_account.lamports, page_account.data.len()));

    let pool_header = decode_account_header::<PoolState>(&pool_account.data).unwrap();
    let page_header = decode_account_header::<QueuePageState>(&page_account.data).unwrap();
    assert_eq!(pool_header.bump, fixture.pool_bump);
    assert_eq!(page_header.bump, fixture.page_bump);
    let state = decode_account::<PoolState>(&pool_account.data).unwrap();
    assert_eq!(state.risk_pool_id.get(), RISK_POOL_ID);
    assert_eq!(state.quote_mint, Key32::new(fixture.quote_mint.to_bytes()));
    assert_eq!(state.admin, Key32::new(fixture.admin.pubkey().to_bytes()));
    let (vault_authority, _) = derive_vault_authority(&id(), &fixture.pool);
    let canonical_vault = derive_canonical_vault(&vault_authority, &fixture.quote_mint);
    assert_eq!(state.vault, Key32::new(canonical_vault.to_bytes()));
    assert!(fixture.svm.get_account(&canonical_vault).is_none());
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );
    assert_eq!(
        state.max_position_notional.get(),
        V1_MAX_POSITION_NOTIONAL_QUOTE
    );
    assert_eq!(
        state.max_net_profit_per_position.get(),
        V1_MAX_NET_PROFIT_PER_POSITION_QUOTE
    );
    assert_eq!(
        state.max_reserved_profit_per_controller.get(),
        V1_MAX_RESERVED_PROFIT_PER_CONTROLLER_QUOTE
    );
    assert_eq!(
        state.total_liability_open_gate.get(),
        V1_TOTAL_POTENTIAL_LIABILITY_QUOTE
    );
    assert_eq!(state.protected_principal.get(), 0);
    assert_eq!(state.free_cash.get(), 0);
    assert_eq!(state.claim_service_cash.get(), 0);
    assert_eq!(state.idle_cash_collateral.get(), 0);
    assert_eq!(state.unallocated_vault_surplus.get(), 0);
    assert_eq!(state.total_queued.get(), 0);
    assert_eq!(state.open_claim_collateral.get(), 0);
    assert_eq!(state.reserved_open_profit.get(), 0);
    assert_eq!(state.residual_bad_debt.get(), 0);
    assert_eq!(state.fee_accrual.get(), 0);
    assert_eq!(state.next_claim_sequence.get(), 1);
    assert_eq!(state.head_claim_sequence.get(), NO_CLAIM_SEQUENCE);
    assert_eq!(state.tail_claim_sequence.get(), NO_CLAIM_SEQUENCE);
    validate_queue_page_zero_account(
        &page_account.data,
        fixture.page_bump,
        Key32::new(fixture.pool.to_bytes()),
    )
    .unwrap();

    let before_pool = pool_account;
    let before_page = page_account;
    assert!(fixture.send(vec![fixture.initialize()], true).is_err());
    assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), before_pool);
    assert_eq!(
        fixture.svm.get_account(&fixture.page_zero).unwrap(),
        before_page
    );
}

#[test]
fn prepare_only_is_inert_and_dust_prefunding_cannot_block_initialization() {
    let mut fixture = Fixture::new(20);
    fixture.svm.airdrop(&fixture.pool, 1).unwrap();
    fixture.svm.airdrop(&fixture.page_zero, 2).unwrap();
    fixture
        .send(vec![fixture.prepare()], true)
        .expect("prepare dust-prefunded PDAs");

    let prepared_pool = fixture.svm.get_account(&fixture.pool).unwrap();
    let prepared_page = fixture.svm.get_account(&fixture.page_zero).unwrap();
    assert_eq!(prepared_pool.owner, id());
    assert_eq!(prepared_page.owner, id());
    assert_eq!(prepared_pool.data.len(), account_len::<PoolState>());
    assert_eq!(prepared_page.data.len(), account_len::<QueuePageState>());
    assert!(prepared_pool.data.iter().all(|byte| *byte == 0));
    assert!(prepared_page.data.iter().all(|byte| *byte == 0));

    fixture
        .send(vec![fixture.initialize()], true)
        .expect("initialize prepared PDAs with fresh account views");
    assert!(
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).is_ok()
    );
}

#[test]
fn failed_initialize_in_bundle_rolls_back_both_allocations_and_admin_debit() {
    let mut fixture = Fixture::new(30);
    let admin_before = fixture.svm.get_account(&fixture.admin.pubkey()).unwrap();
    let wrong_initialize = fixture.instruction(
        MartingalerInstruction::InitializePool {
            risk_pool_id: RISK_POOL_ID + 1,
        },
        fixture.metas(),
    );
    assert!(fixture
        .send(vec![fixture.prepare(), wrong_initialize], true)
        .is_err());
    assert!(fixture.svm.get_account(&fixture.pool).is_none());
    assert!(fixture.svm.get_account(&fixture.page_zero).is_none());
    assert_eq!(
        fixture.svm.get_account(&fixture.admin.pubkey()).unwrap(),
        admin_before
    );
}

#[test]
fn hostile_privileges_pdas_aliases_and_account_counts_fail_before_mutation() {
    let mut cases: Vec<Box<dyn Fn(&Fixture) -> Vec<AccountMeta>>> = vec![
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts[0] = AccountMeta::new(fixture.admin.pubkey(), false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts[0] = AccountMeta::new_readonly(fixture.admin.pubkey(), true);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts[1] = AccountMeta::new(fixture.quote_mint, false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts[2] = AccountMeta::new_readonly(fixture.pool, false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts[3] = AccountMeta::new_readonly(fixture.page_zero, false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts.swap(2, 3);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts[3] = AccountMeta::new(fixture.pool, false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts[2] = AccountMeta::new(Pubkey::new_unique(), false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts[3] = AccountMeta::new(Pubkey::new_unique(), false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts[4] = AccountMeta::new_readonly(Pubkey::new_unique(), false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts.push(AccountMeta::new_readonly(Pubkey::new_unique(), false));
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.metas();
            accounts.pop();
            accounts
        }),
    ];

    for (index, make_accounts) in cases.drain(..).enumerate() {
        let mut fixture = Fixture::new(40 + index as u8);
        let accounts = make_accounts(&fixture);
        let instruction = fixture.instruction(
            MartingalerInstruction::PreparePool {
                risk_pool_id: RISK_POOL_ID,
            },
            accounts,
        );
        let admin_signs = index != 0;
        assert!(fixture.send(vec![instruction], admin_signs).is_err());
        assert!(fixture.svm.get_account(&fixture.pool).is_none());
        assert!(fixture.svm.get_account(&fixture.page_zero).is_none());
    }
}

#[test]
fn malformed_quote_mints_and_wrong_target_owner_fail_closed() {
    for mut fixture in [
        Fixture::new_with_mint(60, 5, true, spl_token::ID, Mint::LEN),
        Fixture::new_with_mint(61, V1_QUOTE_DECIMALS, false, spl_token::ID, Mint::LEN),
        Fixture::new_with_mint(62, V1_QUOTE_DECIMALS, true, system_program::ID, Mint::LEN),
        Fixture::new_with_mint(63, V1_QUOTE_DECIMALS, true, spl_token::ID, Mint::LEN - 1),
    ] {
        assert!(fixture.send(vec![fixture.prepare()], true).is_err());
        assert!(fixture.svm.get_account(&fixture.pool).is_none());
        assert!(fixture.svm.get_account(&fixture.page_zero).is_none());
    }

    let mut fixture = Fixture::new(64);
    fixture
        .svm
        .set_account(
            fixture.pool,
            Account {
                lamports: 1,
                data: Vec::new(),
                owner: Pubkey::new_unique(),
                executable: false,
                rent_epoch: 0,
            },
        )
        .expect("install wrong-owner pool fixture");
    assert!(fixture.send(vec![fixture.prepare()], true).is_err());
    assert_eq!(
        fixture.svm.get_account(&fixture.pool).unwrap().data.len(),
        0
    );
    assert!(fixture.svm.get_account(&fixture.page_zero).is_none());

    let mut fixture = Fixture::new(65);
    fixture
        .svm
        .set_account(
            fixture.pool,
            Account {
                lamports: Rent::default().minimum_balance(1),
                data: vec![0],
                owner: system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .expect("install nonzero-data pool fixture");
    assert!(fixture.send(vec![fixture.prepare()], true).is_err());
    assert_eq!(
        fixture.svm.get_account(&fixture.pool).unwrap().data,
        vec![0]
    );
    assert!(fixture.svm.get_account(&fixture.page_zero).is_none());
}

#[test]
fn malformed_namespaced_data_fails_without_allocating_accounts() {
    let mut fixture = Fixture::new(70);
    let mut data = MartingalerInstruction::PreparePool {
        risk_pool_id: RISK_POOL_ID,
    }
    .encode()
    .to_vec();
    data.push(0);
    let instruction = SolInstruction {
        program_id: id(),
        accounts: fixture.metas(),
        data,
    };
    assert!(fixture.send(vec![instruction], true).is_err());
    assert!(fixture.svm.get_account(&fixture.pool).is_none());
    assert!(fixture.svm.get_account(&fixture.page_zero).is_none());
}

#[test]
fn market_and_controller_bundles_create_exact_empty_roots_and_reject_replay() {
    let mut fixture = Fixture::new(80);
    fixture.send_bundle().expect("initialize canonical pool");
    let pool_before = fixture.svm.get_account(&fixture.pool).unwrap();

    let market_result = fixture
        .send(
            vec![
                fixture.market_instruction(true),
                fixture.market_instruction(false),
            ],
            true,
        )
        .expect("register market bundle");
    eprintln!(
        "martingaler_phase2b2_market_bundle_compute_units={}",
        market_result.compute_units_consumed
    );
    assert!(market_result.compute_units_consumed > 0);
    assert!(market_result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let controller_result = fixture
        .send_controller(vec![
            fixture.controller_instruction(true),
            fixture.controller_instruction(false),
        ])
        .expect("register controller bundle");
    eprintln!(
        "martingaler_phase2b2_controller_bundle_compute_units={}",
        controller_result.compute_units_consumed
    );
    assert!(controller_result.compute_units_consumed > 0);
    assert!(controller_result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), pool_before);

    let market_account = fixture.svm.get_account(&fixture.market).unwrap();
    assert_eq!(market_account.owner, id());
    assert_eq!(market_account.data.len(), account_len::<MarketRiskState>());
    assert!(Rent::default().is_exempt(market_account.lamports, market_account.data.len()));
    assert_eq!(
        decode_account_header::<MarketRiskState>(&market_account.data)
            .unwrap()
            .bump,
        fixture.market_bump
    );
    let market = decode_account::<MarketRiskState>(&market_account.data).unwrap();
    assert_eq!(market.pool, Key32::new(fixture.pool.to_bytes()));
    assert_eq!(market.market_id.get(), MARKET_ID);
    assert_eq!(market.status, PoolStatus::Active as u8);
    assert_eq!(market.max_leverage_x.get(), 100);
    assert_eq!(
        market.max_position_notional.get(),
        V1_MAX_POSITION_NOTIONAL_QUOTE
    );
    assert_eq!(
        market.max_net_profit_per_position.get(),
        V1_MAX_NET_PROFIT_PER_POSITION_QUOTE
    );
    assert_eq!(market.divergence_limit_bps.get(), DIVERGENCE_LIMIT_BPS);
    assert_eq!(
        market.max_long_open_interest.get(),
        V1_TOTAL_POTENTIAL_LIABILITY_QUOTE
    );
    assert_eq!(
        market.max_short_open_interest.get(),
        V1_TOTAL_POTENTIAL_LIABILITY_QUOTE
    );
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.short_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);

    let controller_account = fixture.svm.get_account(&fixture.controller_risk).unwrap();
    assert_eq!(controller_account.owner, id());
    assert_eq!(
        controller_account.data.len(),
        account_len::<ControllerRiskState>()
    );
    assert!(Rent::default().is_exempt(controller_account.lamports, controller_account.data.len()));
    assert_eq!(
        decode_account_header::<ControllerRiskState>(&controller_account.data)
            .unwrap()
            .bump,
        fixture.controller_bump
    );
    let controller = decode_account::<ControllerRiskState>(&controller_account.data).unwrap();
    assert_eq!(controller.pool, Key32::new(fixture.pool.to_bytes()));
    assert_eq!(
        controller.controller,
        Key32::new(fixture.controller.pubkey().to_bytes())
    );
    assert_eq!(controller.available_cash_collateral.get(), 0);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.queued_claim_face.get(), 0);
    assert_eq!(controller.claim_collateral_in_use.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(controller.active_claims.get(), 0);

    let market_before = market_account;
    assert!(fixture
        .send(vec![fixture.market_instruction(false)], true)
        .is_err());
    assert_eq!(
        fixture.svm.get_account(&fixture.market).unwrap(),
        market_before
    );
    let controller_before = controller_account;
    assert!(fixture
        .send_controller(vec![fixture.controller_instruction(false)])
        .is_err());
    assert_eq!(
        fixture.svm.get_account(&fixture.controller_risk).unwrap(),
        controller_before
    );
}

#[test]
fn registration_prepare_is_inert_and_dust_prefunding_is_tolerated() {
    let mut fixture = Fixture::new(81);
    fixture.send_bundle().expect("initialize canonical pool");
    fixture.svm.airdrop(&fixture.market, 1).unwrap();
    fixture.svm.airdrop(&fixture.controller_risk, 2).unwrap();

    fixture
        .send(vec![fixture.market_instruction(true)], true)
        .expect("prepare dust-prefunded market");
    fixture
        .send_controller(vec![fixture.controller_instruction(true)])
        .expect("prepare dust-prefunded controller");

    let market = fixture.svm.get_account(&fixture.market).unwrap();
    let controller = fixture.svm.get_account(&fixture.controller_risk).unwrap();
    assert_eq!(market.owner, id());
    assert_eq!(controller.owner, id());
    assert_eq!(market.data.len(), account_len::<MarketRiskState>());
    assert_eq!(controller.data.len(), account_len::<ControllerRiskState>());
    assert!(market.data.iter().all(|byte| *byte == 0));
    assert!(controller.data.iter().all(|byte| *byte == 0));
    assert!(decode_account::<MarketRiskState>(&market.data).is_err());
    assert!(decode_account::<ControllerRiskState>(&controller.data).is_err());

    fixture
        .send(vec![fixture.market_instruction(false)], true)
        .expect("initialize prepared market");
    fixture
        .send_controller(vec![fixture.controller_instruction(false)])
        .expect("initialize prepared controller");
    assert!(decode_account::<MarketRiskState>(
        &fixture.svm.get_account(&fixture.market).unwrap().data
    )
    .is_ok());
    assert!(decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data
    )
    .is_ok());
}

#[test]
fn registration_authority_privilege_pda_and_account_shape_checks_fail_closed() {
    let market_cases: Vec<Box<dyn Fn(&Fixture) -> Vec<AccountMeta>>> = vec![
        Box::new(|fixture| {
            let mut accounts = fixture.market_metas();
            accounts[0] = AccountMeta::new(fixture.admin.pubkey(), false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.market_metas();
            accounts[0] = AccountMeta::new_readonly(fixture.admin.pubkey(), true);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.market_metas();
            accounts[1] = AccountMeta::new(fixture.pool, false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.market_metas();
            accounts[2] = AccountMeta::new_readonly(fixture.market, false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.market_metas();
            accounts[2] = AccountMeta::new(Pubkey::new_unique(), false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.market_metas();
            accounts[2] = AccountMeta::new(fixture.pool, false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.market_metas();
            accounts[3] = AccountMeta::new_readonly(Pubkey::new_unique(), false);
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.market_metas();
            accounts.push(AccountMeta::new_readonly(Pubkey::new_unique(), false));
            accounts
        }),
        Box::new(|fixture| {
            let mut accounts = fixture.market_metas();
            accounts.pop();
            accounts
        }),
    ];

    for (index, accounts) in market_cases.into_iter().enumerate() {
        let mut fixture = Fixture::new(90 + index as u8);
        fixture.send_bundle().expect("initialize canonical pool");
        let instruction = fixture.instruction(
            MartingalerInstruction::PrepareMarket {
                market_id: MARKET_ID,
            },
            accounts(&fixture),
        );
        assert!(fixture.send(vec![instruction], index != 0).is_err());
        assert!(fixture.svm.get_account(&fixture.market).is_none());
    }

    let mut wrong_admin = Fixture::new(101);
    wrong_admin
        .send_bundle()
        .expect("initialize canonical pool");
    let mut wrong_admin_metas = wrong_admin.market_metas();
    wrong_admin_metas[0] = AccountMeta::new(wrong_admin.controller.pubkey(), true);
    let instruction = wrong_admin.instruction(
        MartingalerInstruction::PrepareMarket {
            market_id: MARKET_ID,
        },
        wrong_admin_metas,
    );
    assert!(wrong_admin.send_controller(vec![instruction]).is_err());
    assert!(wrong_admin.svm.get_account(&wrong_admin.market).is_none());

    let mut controller = Fixture::new(102);
    controller.send_bundle().expect("initialize canonical pool");
    let mut missing_signature_metas = controller.controller_metas();
    missing_signature_metas[0] = AccountMeta::new(controller.controller.pubkey(), false);
    let missing_signature = controller.instruction(
        MartingalerInstruction::PrepareController,
        missing_signature_metas,
    );
    assert!(controller.send(vec![missing_signature], false).is_err());
    assert!(controller
        .svm
        .get_account(&controller.controller_risk)
        .is_none());

    let mut wrong_controller_metas = controller.controller_metas();
    wrong_controller_metas[0] = AccountMeta::new(controller.admin.pubkey(), true);
    let wrong_controller = controller.instruction(
        MartingalerInstruction::PrepareController,
        wrong_controller_metas,
    );
    assert!(controller.send(vec![wrong_controller], true).is_err());
    assert!(controller
        .svm
        .get_account(&controller.controller_risk)
        .is_none());
}

#[test]
fn registration_limits_pool_integrity_and_atomic_rollback_fail_closed() {
    for instruction in [
        MartingalerInstruction::PrepareMarket { market_id: 0 },
        MartingalerInstruction::InitializeMarket {
            market_id: MARKET_ID,
            divergence_limit_bps: 0,
            max_long_open_interest: 1,
            max_short_open_interest: 1,
            haircut: haircut(),
        },
        MartingalerInstruction::InitializeMarket {
            market_id: MARKET_ID,
            divergence_limit_bps: 10_001,
            max_long_open_interest: 1,
            max_short_open_interest: 1,
            haircut: haircut(),
        },
        MartingalerInstruction::InitializeMarket {
            market_id: MARKET_ID,
            divergence_limit_bps: DIVERGENCE_LIMIT_BPS,
            max_long_open_interest: 0,
            max_short_open_interest: 1,
            haircut: haircut(),
        },
        MartingalerInstruction::InitializeMarket {
            market_id: MARKET_ID,
            divergence_limit_bps: DIVERGENCE_LIMIT_BPS,
            max_long_open_interest: 1,
            max_short_open_interest: V1_TOTAL_POTENTIAL_LIABILITY_QUOTE + 1,
            haircut: haircut(),
        },
    ] {
        assert!(MartingalerInstruction::decode(&instruction.encode()).is_err());
    }

    let mut fixture = Fixture::new(103);
    fixture.send_bundle().expect("initialize canonical pool");
    let admin_before = fixture.svm.get_account(&fixture.admin.pubkey()).unwrap();
    let wrong_initialize = fixture.instruction(
        MartingalerInstruction::InitializeMarket {
            market_id: MARKET_ID + 1,
            divergence_limit_bps: DIVERGENCE_LIMIT_BPS,
            max_long_open_interest: V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
            max_short_open_interest: V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
            haircut: haircut(),
        },
        fixture.market_metas(),
    );
    assert!(fixture
        .send(
            vec![fixture.market_instruction(true), wrong_initialize],
            true,
        )
        .is_err());
    assert!(fixture.svm.get_account(&fixture.market).is_none());
    assert_eq!(
        fixture.svm.get_account(&fixture.admin.pubkey()).unwrap(),
        admin_before
    );

    let mut corrupt_pool = Fixture::new(104);
    corrupt_pool
        .send_bundle()
        .expect("initialize canonical pool");
    let mut pool_account = corrupt_pool.svm.get_account(&corrupt_pool.pool).unwrap();
    pool_account.data[12] ^= 1;
    corrupt_pool
        .svm
        .set_account(corrupt_pool.pool, pool_account)
        .unwrap();
    assert!(corrupt_pool
        .send(vec![corrupt_pool.market_instruction(true)], true)
        .is_err());
    assert!(corrupt_pool.svm.get_account(&corrupt_pool.market).is_none());

    let mut non_rent_exempt = Fixture::new(105);
    non_rent_exempt
        .send_bundle()
        .expect("initialize canonical pool");
    non_rent_exempt
        .send(vec![non_rent_exempt.market_instruction(true)], true)
        .expect("prepare market");
    let mut market_account = non_rent_exempt
        .svm
        .get_account(&non_rent_exempt.market)
        .unwrap();
    market_account.lamports = 1;
    non_rent_exempt
        .svm
        .set_account(non_rent_exempt.market, market_account)
        .unwrap();
    assert!(non_rent_exempt
        .send(vec![non_rent_exempt.market_instruction(false)], true)
        .is_err());
    assert!(decode_account::<MarketRiskState>(
        &non_rent_exempt
            .svm
            .get_account(&non_rent_exempt.market)
            .unwrap()
            .data
    )
    .is_err());

    let mut controller_rollback = Fixture::new(106);
    controller_rollback
        .send_bundle()
        .expect("initialize canonical pool");
    let controller_before = controller_rollback
        .svm
        .get_account(&controller_rollback.controller.pubkey())
        .unwrap();
    let mut malformed_initialize = controller_rollback.controller_instruction(false);
    malformed_initialize.data.push(0);
    assert!(controller_rollback
        .send_controller(vec![
            controller_rollback.controller_instruction(true),
            malformed_initialize,
        ])
        .is_err());
    assert!(controller_rollback
        .svm
        .get_account(&controller_rollback.controller_risk)
        .is_none());
    assert_eq!(
        controller_rollback
            .svm
            .get_account(&controller_rollback.controller.pubkey())
            .unwrap(),
        controller_before
    );

    let mut corrupt_vault = Fixture::new(107);
    corrupt_vault
        .send_bundle()
        .expect("initialize canonical pool");
    let mut pool_account = corrupt_vault.svm.get_account(&corrupt_vault.pool).unwrap();
    pool_account.data[56..88].fill(9);
    corrupt_vault
        .svm
        .set_account(corrupt_vault.pool, pool_account)
        .unwrap();
    assert!(corrupt_vault
        .send(vec![corrupt_vault.market_instruction(true)], true)
        .is_err());
    assert!(corrupt_vault
        .svm
        .get_account(&corrupt_vault.market)
        .is_none());

    let mut inactive_pool = Fixture::new(108);
    inactive_pool
        .send_bundle()
        .expect("initialize canonical pool");
    let mut pool_account = inactive_pool.svm.get_account(&inactive_pool.pool).unwrap();
    pool_account.data[120] = PoolStatus::CloseOnly as u8;
    inactive_pool
        .svm
        .set_account(inactive_pool.pool, pool_account)
        .unwrap();
    assert!(inactive_pool
        .send(vec![inactive_pool.market_instruction(true)], true)
        .is_err());
    assert!(inactive_pool
        .svm
        .get_account(&inactive_pool.market)
        .is_none());

    let mut wrong_pool_owner = Fixture::new(109);
    wrong_pool_owner
        .send_bundle()
        .expect("initialize canonical pool");
    let mut pool_account = wrong_pool_owner
        .svm
        .get_account(&wrong_pool_owner.pool)
        .unwrap();
    pool_account.owner = system_program::ID;
    wrong_pool_owner
        .svm
        .set_account(wrong_pool_owner.pool, pool_account)
        .unwrap();
    assert!(wrong_pool_owner
        .send(vec![wrong_pool_owner.market_instruction(true)], true)
        .is_err());
    assert!(wrong_pool_owner
        .svm
        .get_account(&wrong_pool_owner.market)
        .is_none());

    let mut wrong_pool_length = Fixture::new(110);
    wrong_pool_length
        .send_bundle()
        .expect("initialize canonical pool");
    let mut pool_account = wrong_pool_length
        .svm
        .get_account(&wrong_pool_length.pool)
        .unwrap();
    pool_account.data.pop();
    wrong_pool_length
        .svm
        .set_account(wrong_pool_length.pool, pool_account)
        .unwrap();
    assert!(wrong_pool_length
        .send(vec![wrong_pool_length.market_instruction(true)], true)
        .is_err());
    assert!(wrong_pool_length
        .svm
        .get_account(&wrong_pool_length.market)
        .is_none());

    let mut nonzero_prepared = Fixture::new(111);
    nonzero_prepared
        .send_bundle()
        .expect("initialize canonical pool");
    nonzero_prepared
        .send(vec![nonzero_prepared.market_instruction(true)], true)
        .expect("prepare market");
    let mut market_account = nonzero_prepared
        .svm
        .get_account(&nonzero_prepared.market)
        .unwrap();
    market_account.data[16] = 1;
    nonzero_prepared
        .svm
        .set_account(nonzero_prepared.market, market_account)
        .unwrap();
    assert!(nonzero_prepared
        .send(vec![nonzero_prepared.market_instruction(false)], true)
        .is_err());
    assert!(decode_account::<MarketRiskState>(
        &nonzero_prepared
            .svm
            .get_account(&nonzero_prepared.market)
            .unwrap()
            .data
    )
    .is_err());
}

#[test]
fn canonical_vault_initialization_is_idempotent_and_dust_tolerant() {
    let mut fixture = Fixture::new(120);
    fixture.send_bundle().expect("initialize canonical pool");
    let pool_before = fixture.svm.get_account(&fixture.pool).unwrap();
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();

    let result = fixture
        .send(vec![fixture.vault_instruction()], true)
        .expect("initialize canonical vault");
    eprintln!(
        "martingaler_phase2b3_vault_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed > 0);
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));
    let vault_before_replay = fixture.svm.get_account(&fixture.vault).unwrap();
    assert_eq!(vault_before_replay.owner, spl_token::ID);
    assert!(Rent::default().is_exempt(vault_before_replay.lamports, vault_before_replay.data.len()));
    let vault_state = TokenAccount::unpack(&vault_before_replay.data).unwrap();
    assert_eq!(vault_state.mint, fixture.quote_mint);
    assert_eq!(vault_state.owner, fixture.vault_authority);
    assert_eq!(vault_state.amount, 0);
    assert_eq!(vault_state.delegate, COption::None);
    assert_eq!(vault_state.close_authority, COption::None);
    assert_eq!(vault_state.is_native, COption::None);
    assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), pool_before);
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );

    fixture.svm.expire_blockhash();
    fixture
        .send(vec![fixture.vault_instruction()], true)
        .expect("idempotent vault replay");
    assert_eq!(
        fixture.svm.get_account(&fixture.vault).unwrap(),
        vault_before_replay
    );

    let mut dust = Fixture::new(121);
    dust.send_bundle().expect("initialize dust fixture pool");
    dust.svm
        .airdrop(&dust.vault, 1)
        .expect("dust canonical vault");
    dust.send(vec![dust.vault_instruction()], true)
        .expect("initialize dust-prefunded canonical vault");
    let dust_vault =
        TokenAccount::unpack(&dust.svm.get_account(&dust.vault).unwrap().data).unwrap();
    assert_eq!(dust_vault.mint, dust.quote_mint);
    assert_eq!(dust_vault.owner, dust.vault_authority);
}

#[test]
fn hostile_vault_initialization_accounts_fail_without_mutation() {
    let mut fixture = Fixture::new(126);
    fixture.send_bundle().expect("initialize canonical pool");
    let pool_before = fixture.svm.get_account(&fixture.pool).unwrap();
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();

    let mut wrong_admin = fixture.vault_metas();
    wrong_admin[0] = AccountMeta::new(fixture.controller.pubkey(), true);
    let instruction = fixture.instruction(MartingalerInstruction::InitializeVault, wrong_admin);
    assert!(fixture.send_controller(vec![instruction]).is_err());

    for accounts in [
        {
            let mut accounts = fixture.vault_metas();
            accounts[3] = AccountMeta::new_readonly(fixture.page_zero, false);
            accounts
        },
        {
            let mut accounts = fixture.vault_metas();
            accounts[4] = AccountMeta::new(fixture.page_zero, false);
            accounts
        },
        {
            let mut accounts = fixture.vault_metas();
            accounts[5] = AccountMeta::new_readonly(id(), false);
            accounts
        },
        {
            let mut accounts = fixture.vault_metas();
            accounts[6] = AccountMeta::new_readonly(id(), false);
            accounts
        },
        {
            let mut accounts = fixture.vault_metas();
            accounts[7] = AccountMeta::new_readonly(id(), false);
            accounts
        },
        {
            let mut accounts = fixture.vault_metas();
            accounts.pop();
            accounts
        },
        {
            let mut accounts = fixture.vault_metas();
            accounts.push(AccountMeta::new_readonly(id(), false));
            accounts
        },
    ] {
        let instruction = fixture.instruction(MartingalerInstruction::InitializeVault, accounts);
        assert!(fixture.send(vec![instruction], true).is_err());
        assert!(fixture.svm.get_account(&fixture.vault).is_none());
        assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), pool_before);
        assert_eq!(
            fixture.svm.get_account(&fixture.quote_mint).unwrap(),
            mint_before
        );
    }
}

#[test]
fn cash_deposit_moves_exact_tokens_and_credits_only_owned_idle_collateral() {
    let mut fixture = Fixture::new(122);
    fixture.send_bundle().expect("initialize canonical pool");
    fixture
        .send_controller(vec![
            fixture.controller_instruction(true),
            fixture.controller_instruction(false),
        ])
        .expect("register controller");
    fixture
        .send(vec![fixture.vault_instruction()], true)
        .expect("initialize canonical vault");
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();

    let result = fixture
        .send_controller(vec![fixture.deposit_instruction(DEPOSIT_AMOUNT)])
        .expect("deposit controller cash");
    eprintln!(
        "martingaler_phase2b3_deposit_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed > 0);
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let source = TokenAccount::unpack(&fixture.svm.get_account(&fixture.source).unwrap().data)
        .expect("decode source");
    let vault = TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data)
        .expect("decode vault");
    let pool = decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data)
        .expect("decode pool");
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .expect("decode controller");
    assert_eq!(source.amount, SOURCE_BALANCE - DEPOSIT_AMOUNT);
    assert_eq!(vault.amount, DEPOSIT_AMOUNT);
    assert_eq!(pool.idle_cash_collateral.get(), u128::from(DEPOSIT_AMOUNT));
    assert_eq!(
        controller.available_cash_collateral.get(),
        u128::from(DEPOSIT_AMOUNT)
    );
    assert_eq!(pool.unallocated_vault_surplus.get(), 0);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.free_cash.get(), 0);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault.amount)
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );
}

#[test]
fn dual_oracle_cash_open_consumes_idle_cash_once_and_records_exact_provenance() {
    let mut fixture = Fixture::new(128);
    bootstrap_cash_open(&mut fixture, true);
    let primary = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let secondary =
        fixture.install_switchboard_price(&SECONDARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    const OPEN_TIME_DONATION: u64 = 7_000_000;
    let mut donated_vault = fixture.svm.get_account(&fixture.vault).unwrap();
    let mut donated_vault_state = TokenAccount::unpack(&donated_vault.data).unwrap();
    donated_vault_state.amount = donated_vault_state
        .amount
        .checked_add(OPEN_TIME_DONATION)
        .unwrap();
    TokenAccount::pack(donated_vault_state, &mut donated_vault.data).unwrap();
    fixture
        .svm
        .set_account(fixture.vault, donated_vault)
        .unwrap();
    let source_before = fixture.svm.get_account(&fixture.source).unwrap();
    let vault_before = fixture.svm.get_account(&fixture.vault).unwrap();
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();

    let result = fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(
                primary,
                Some(secondary),
                OPEN_COLLATERAL,
                OPEN_NOTIONAL,
                100,
            ),
        ])
        .expect("open dual-oracle cash position");
    eprintln!(
        "martingaler_phase2b4_cash_open_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed > 0);
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let market =
        decode_account::<MarketRiskState>(&fixture.svm.get_account(&fixture.market).unwrap().data)
            .unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let position_account = fixture.svm.get_account(&fixture.position).unwrap();
    let position = decode_account::<PositionState>(&position_account.data).unwrap();
    assert_eq!(
        decode_account_header::<PositionState>(&position_account.data)
            .unwrap()
            .bump,
        fixture.position_bump
    );
    assert_eq!(
        pool.idle_cash_collateral.get(),
        u128::from(DEPOSIT_AMOUNT - OPEN_COLLATERAL)
    );
    assert_eq!(pool.protected_principal.get(), u128::from(OPEN_COLLATERAL));
    assert_eq!(pool.reserved_open_profit.get(), u128::from(OPEN_NOTIONAL));
    assert_eq!(
        pool.unallocated_vault_surplus.get(),
        u128::from(OPEN_TIME_DONATION)
    );
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(DEPOSIT_AMOUNT + OPEN_TIME_DONATION)
    );
    assert_eq!(market.long_open_interest.get(), u128::from(OPEN_NOTIONAL));
    assert_eq!(market.short_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), u128::from(OPEN_NOTIONAL));
    assert_eq!(
        controller.available_cash_collateral.get(),
        u128::from(DEPOSIT_AMOUNT - OPEN_COLLATERAL)
    );
    assert_eq!(controller.reserved_profit.get(), u128::from(OPEN_NOTIONAL));
    assert_eq!(controller.open_positions.get(), 1);
    assert_eq!(position.pool, Key32::new(fixture.pool.to_bytes()));
    assert_eq!(
        position.controller,
        Key32::new(fixture.controller.pubkey().to_bytes())
    );
    assert_eq!(position.position_id.get(), POSITION_ID);
    assert_eq!(position.market_id.get(), MARKET_ID);
    assert_eq!(position.side, PositionSide::Long as u8);
    assert_eq!(position.collateral.get(), u128::from(OPEN_COLLATERAL));
    assert_eq!(position.notional.get(), u128::from(OPEN_NOTIONAL));
    assert_eq!(position.max_payable_profit.get(), u128::from(OPEN_NOTIONAL));
    assert_eq!(position.entry_price_e6.get(), 100_000_000);
    assert_eq!(position.entry_publish_time.get(), ORACLE_TIME);
    assert_eq!(position.secondary_entry_price_e6.get(), 101_000_000);
    assert_eq!(position.secondary_publish_time.get(), ORACLE_TIME);
    assert_eq!(
        fixture.svm.get_account(&fixture.source).unwrap(),
        source_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.vault).unwrap(),
        vault_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );
}

#[test]
fn voluntary_dual_oracle_cash_close_requires_policy_parity_and_succeeds_atomically() {
    let mut fixture = Fixture::new(129);
    bootstrap_cash_open(&mut fixture, true);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let entry_secondary =
        fixture.install_switchboard_price(&SECONDARY_FEED_ID, 100_200_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(
                entry,
                Some(entry_secondary),
                OPEN_COLLATERAL,
                OPEN_NOTIONAL,
                100,
            ),
        ])
        .expect("open dual-oracle position");

    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 99_500_000, 100_000, ORACLE_TIME);
    let exit_secondary =
        fixture.install_switchboard_price(&SECONDARY_FEED_ID, 99_700_000, 100_000, ORACLE_TIME);
    let result = fixture
        .send_controller(vec![
            fixture.close_cash_instruction_with_oracles(exit, Some(exit_secondary))
        ])
        .expect("close dual-oracle position with agreeing feeds");
    eprintln!(
        "martingaler_dual_oracle_voluntary_cash_close_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let position =
        decode_account::<PositionState>(&fixture.svm.get_account(&fixture.position).unwrap().data)
            .unwrap();
    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.residual_bad_debt.get(), 0);
}

#[test]
fn voluntary_dual_oracle_cash_close_rejects_missing_duplicate_wrong_and_divergent_secondary() {
    for case in 0u8..5 {
        let mut fixture = Fixture::new(130 + case);
        bootstrap_cash_open(&mut fixture, true);
        let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
        let entry_secondary = fixture.install_switchboard_price(
            &SECONDARY_FEED_ID,
            100_200_000,
            100_000,
            ORACLE_TIME,
        );
        fixture
            .send_controller(vec![
                fixture.prepare_position_instruction(),
                fixture.open_cash_instruction(
                    entry,
                    Some(entry_secondary),
                    OPEN_COLLATERAL,
                    OPEN_NOTIONAL,
                    100,
                ),
            ])
            .unwrap();

        let primary =
            fixture.install_pyth_price(&PRIMARY_FEED_ID, 99_500_000, 100_000, ORACLE_TIME);
        let secondary = match case {
            1 => primary,
            2 => fixture.install_pyth_price(&SECONDARY_FEED_ID, 99_700_000, 100_000, ORACLE_TIME),
            3 => fixture.install_switchboard_price(&[0x55; 32], 99_700_000, 100_000, ORACLE_TIME),
            4 => fixture.install_switchboard_price(
                &SECONDARY_FEED_ID,
                110_000_000,
                100_000,
                ORACLE_TIME,
            ),
            _ => fixture.install_switchboard_price(
                &SECONDARY_FEED_ID,
                99_700_000,
                100_000,
                ORACLE_TIME,
            ),
        };
        let secondary_arg = (case != 0).then_some(secondary);
        let instruction = fixture.close_cash_instruction_with_oracles(primary, secondary_arg);
        let before = cash_open_snapshot(&fixture, &[primary, secondary]);
        assert!(
            fixture.send_controller(vec![instruction]).is_err(),
            "dual voluntary close case {case}"
        );
        assert_eq!(
            cash_open_snapshot(&fixture, &[primary, secondary]),
            before,
            "dual voluntary close case {case} mutated state"
        );
    }
}

#[test]
fn oracle_policy_registration_is_canonical_immutable_and_bounded() {
    let mut fixture = Fixture::new(130);
    fixture.send_bundle().expect("initialize canonical pool");
    fixture
        .send(
            vec![
                fixture.market_instruction(true),
                fixture.market_instruction(false),
            ],
            true,
        )
        .expect("register canonical market");
    let result = fixture
        .send(
            vec![
                fixture.oracle_policy_instruction(true, true),
                fixture.oracle_policy_instruction(false, true),
            ],
            true,
        )
        .expect("register dual oracle policy");
    eprintln!(
        "martingaler_phase2b4_oracle_policy_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed > 0);
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let account = fixture.svm.get_account(&fixture.oracle_policy).unwrap();
    assert_eq!(account.owner, id());
    assert_eq!(account.data.len(), account_len::<OraclePolicyState>());
    assert!(Rent::default().is_exempt(account.lamports, account.data.len()));
    assert_eq!(
        decode_account_header::<OraclePolicyState>(&account.data)
            .unwrap()
            .bump,
        fixture.oracle_policy_bump
    );
    let policy = decode_account::<OraclePolicyState>(&account.data).unwrap();
    assert_eq!(policy.pool, Key32::new(fixture.pool.to_bytes()));
    assert_eq!(policy.market, Key32::new(fixture.market.to_bytes()));
    assert_eq!(policy.primary_feed_id, Key32::new(PRIMARY_FEED_ID));
    assert_eq!(policy.secondary_feed_id, Key32::new(SECONDARY_FEED_ID));
    assert_eq!(policy.oracle_count, 2);
    assert_eq!(policy.max_staleness_secs.get(), 30);
    assert_eq!(policy.confidence_limit_bps.get(), 100);

    let before = account;
    assert!(fixture
        .send(vec![fixture.oracle_policy_instruction(false, false)], true)
        .is_err());
    assert_eq!(
        fixture.svm.get_account(&fixture.oracle_policy).unwrap(),
        before
    );
}

#[test]
fn single_oracle_policy_opens_with_zero_secondary_provenance() {
    let mut fixture = Fixture::new(129);
    bootstrap_cash_open(&mut fixture, false);
    let primary = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(primary, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("open single-oracle cash position");

    let position =
        decode_account::<PositionState>(&fixture.svm.get_account(&fixture.position).unwrap().data)
            .unwrap();
    assert_eq!(position.entry_price_e6.get(), 100_000_000);
    assert_eq!(position.entry_publish_time.get(), ORACLE_TIME);
    assert_eq!(position.secondary_entry_price_e6.get(), 0);
    assert_eq!(position.secondary_publish_time.get(), 0);
}

#[test]
fn stale_future_wrong_feed_owner_verification_confidence_and_divergence_fail_atomically() {
    for case in 0u8..9 {
        let mut fixture = Fixture::new(140 + case);
        bootstrap_cash_open(&mut fixture, true);
        let mut primary_feed = PRIMARY_FEED_ID;
        let mut primary_time = ORACLE_TIME;
        let mut primary_conf = 100_000;
        let mut secondary_price = 101_000_000;
        match case {
            0 => primary_time = ORACLE_TIME - 31,
            1 => primary_time = ORACLE_TIME + 1,
            2 => primary_feed = [0x33; 32],
            3 => primary_conf = 2_000_000,
            4 => secondary_price = 200_000_000,
            5..=8 => {}
            _ => unreachable!(),
        }
        let primary =
            fixture.install_pyth_price(&primary_feed, 100_000_000, primary_conf, primary_time);
        let secondary = if case == 8 {
            fixture.install_pyth_price(
                &SECONDARY_FEED_ID,
                i64::try_from(secondary_price).unwrap(),
                100_000,
                ORACLE_TIME,
            )
        } else {
            fixture.install_switchboard_price(
                &SECONDARY_FEED_ID,
                secondary_price,
                100_000,
                ORACLE_TIME,
            )
        };
        if case == 5 {
            let mut account = fixture.svm.get_account(&primary).unwrap();
            account.owner = system_program::ID;
            fixture.svm.set_account(primary, account).unwrap();
        }
        if case == 6 {
            let mut account = fixture.svm.get_account(&primary).unwrap();
            account.data[40] = 0;
            fixture.svm.set_account(primary, account).unwrap();
        }

        let pool_before = fixture.svm.get_account(&fixture.pool).unwrap();
        let market_before = fixture.svm.get_account(&fixture.market).unwrap();
        let controller_before = fixture.svm.get_account(&fixture.controller_risk).unwrap();
        let vault_before = fixture.svm.get_account(&fixture.vault).unwrap();
        let authority_before = fixture
            .svm
            .get_account(&fixture.controller.pubkey())
            .unwrap();
        let secondary_arg = if case == 7 { None } else { Some(secondary) };
        assert!(fixture
            .send_controller(vec![
                fixture.prepare_position_instruction(),
                fixture.open_cash_instruction(
                    primary,
                    secondary_arg,
                    OPEN_COLLATERAL,
                    OPEN_NOTIONAL,
                    100,
                ),
            ])
            .is_err());
        assert!(fixture.svm.get_account(&fixture.position).is_none());
        assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), pool_before);
        assert_eq!(
            fixture.svm.get_account(&fixture.market).unwrap(),
            market_before
        );
        assert_eq!(
            fixture.svm.get_account(&fixture.controller_risk).unwrap(),
            controller_before
        );
        assert_eq!(
            fixture.svm.get_account(&fixture.vault).unwrap(),
            vault_before
        );
        assert_eq!(
            fixture
                .svm
                .get_account(&fixture.controller.pubkey())
                .unwrap(),
            authority_before
        );
    }

    let mut single = Fixture::new(149);
    bootstrap_cash_open(&mut single, false);
    let primary = single.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let extra = single.install_pyth_price(&SECONDARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    assert!(single
        .send_controller(vec![
            single.prepare_position_instruction(),
            single
                .open_cash_instruction(primary, Some(extra), OPEN_COLLATERAL, OPEN_NOTIONAL, 100,),
        ])
        .is_err());
    assert!(single.svm.get_account(&single.position).is_none());
}

#[test]
fn unsupported_or_same_family_oracle_providers_fail_atomically() {
    let mut chainlink = Fixture::new(150);
    bootstrap_cash_open(&mut chainlink, false);
    let chainlink_primary = Pubkey::new_from_array(PRIMARY_FEED_ID);
    let chainlink_data =
        make_chainlink_data(6, 1, u32::try_from(ORACLE_TIME).unwrap(), 100_000_000);
    let parsed = oracle_v16::read_oracle_price_e6_from_bytes(
        &oracle_v16::CHAINLINK_STORE_PROGRAM_ID,
        &chainlink_primary,
        &chainlink_data,
        &PRIMARY_FEED_ID,
        ORACLE_TIME,
        30,
        100,
    )
    .expect("legacy v16 parser accepts the structurally valid Chainlink observation");
    assert_eq!(parsed, (100_000_000, ORACLE_TIME));
    chainlink
        .svm
        .set_account(
            chainlink_primary,
            Account {
                lamports: 1_000_000_000,
                data: chainlink_data,
                owner: oracle_v16::CHAINLINK_STORE_PROGRAM_ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .expect("install unsupported Chainlink fixture");
    let before = cash_open_snapshot(&chainlink, &[chainlink_primary]);
    assert!(chainlink
        .send_controller(vec![
            chainlink.prepare_position_instruction(),
            chainlink.open_cash_instruction(
                chainlink_primary,
                None,
                OPEN_COLLATERAL,
                OPEN_NOTIONAL,
                100,
            ),
        ])
        .is_err());
    assert_eq!(cash_open_snapshot(&chainlink, &[chainlink_primary]), before);
    assert!(chainlink.svm.get_account(&chainlink.position).is_none());

    let mut same_family = Fixture::new(151);
    bootstrap_cash_open(&mut same_family, true);
    let primary = same_family.install_switchboard_price_with_owner(
        &PRIMARY_FEED_ID,
        100_000_000,
        100_000,
        ORACLE_TIME,
        oracle_v16::SWITCHBOARD_ON_DEMAND_MAINNET_PROGRAM_ID,
    );
    let secondary = same_family.install_switchboard_price_with_owner(
        &SECONDARY_FEED_ID,
        101_000_000,
        100_000,
        ORACLE_TIME,
        oracle_v16::SWITCHBOARD_ON_DEMAND_DEVNET_PROGRAM_ID,
    );
    let before = cash_open_snapshot(&same_family, &[primary, secondary]);
    assert!(same_family
        .send_controller(vec![
            same_family.prepare_position_instruction(),
            same_family.open_cash_instruction(
                primary,
                Some(secondary),
                OPEN_COLLATERAL,
                OPEN_NOTIONAL,
                100,
            ),
        ])
        .is_err());
    assert_eq!(
        cash_open_snapshot(&same_family, &[primary, secondary]),
        before
    );
    assert!(same_family.svm.get_account(&same_family.position).is_none());
}

#[test]
fn cash_open_risk_cash_and_custody_failures_roll_back_the_prepare_bundle() {
    let cases = [
        (DEPOSIT_AMOUNT + 1, DEPOSIT_AMOUNT + 1, 1),
        (OPEN_COLLATERAL, OPEN_NOTIONAL, 101),
        (OPEN_COLLATERAL, OPEN_NOTIONAL, 1),
        (
            OPEN_COLLATERAL,
            u64::try_from(V1_MAX_POSITION_NOTIONAL_QUOTE).unwrap() + 1,
            100,
        ),
    ];
    for (case, (collateral, notional, leverage_x)) in cases.into_iter().enumerate() {
        let mut fixture = Fixture::new(160 + case as u8);
        bootstrap_cash_open(&mut fixture, false);
        let primary =
            fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
        let before = cash_open_snapshot(&fixture, &[primary]);
        assert!(fixture
            .send_controller(vec![
                fixture.prepare_position_instruction(),
                fixture.open_cash_instruction(primary, None, collateral, notional, leverage_x),
            ])
            .is_err());
        assert_eq!(cash_open_snapshot(&fixture, &[primary]), before);
        assert!(fixture.svm.get_account(&fixture.position).is_none());
    }

    let mut shortfall = Fixture::new(164);
    bootstrap_cash_open(&mut shortfall, false);
    let primary = shortfall.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let mut vault_account = shortfall.svm.get_account(&shortfall.vault).unwrap();
    let mut vault_state = TokenAccount::unpack(&vault_account.data).unwrap();
    vault_state.amount -= 1;
    TokenAccount::pack(vault_state, &mut vault_account.data).unwrap();
    shortfall
        .svm
        .set_account(shortfall.vault, vault_account)
        .unwrap();
    let before = cash_open_snapshot(&shortfall, &[primary]);
    assert!(shortfall
        .send_controller(vec![
            shortfall.prepare_position_instruction(),
            shortfall.open_cash_instruction(primary, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100,),
        ])
        .is_err());
    assert_eq!(cash_open_snapshot(&shortfall, &[primary]), before);
    assert!(shortfall.svm.get_account(&shortfall.position).is_none());

    for case in 0u8..3 {
        let mut fixture = Fixture::new(165 + case);
        bootstrap_cash_open(&mut fixture, false);
        let primary =
            fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
        match case {
            0 => {
                let mut account = fixture.svm.get_account(&fixture.market).unwrap();
                let mut state = decode_account::<MarketRiskState>(&account.data).unwrap();
                state.long_open_interest = state.max_long_open_interest;
                overwrite_account(&mut account.data, &state).unwrap();
                fixture.svm.set_account(fixture.market, account).unwrap();
            }
            1 => {
                let pool = decode_account::<PoolState>(
                    &fixture.svm.get_account(&fixture.pool).unwrap().data,
                )
                .unwrap();
                let mut account = fixture.svm.get_account(&fixture.controller_risk).unwrap();
                let mut state = decode_account::<ControllerRiskState>(&account.data).unwrap();
                state.reserved_profit = pool.max_reserved_profit_per_controller;
                overwrite_account(&mut account.data, &state).unwrap();
                fixture
                    .svm
                    .set_account(fixture.controller_risk, account)
                    .unwrap();
            }
            2 => {
                let mut account = fixture.svm.get_account(&fixture.pool).unwrap();
                let mut state = decode_account::<PoolState>(&account.data).unwrap();
                state.residual_bad_debt = state.total_liability_open_gate;
                overwrite_account(&mut account.data, &state).unwrap();
                fixture.svm.set_account(fixture.pool, account).unwrap();
            }
            _ => unreachable!(),
        }
        let before = cash_open_snapshot(&fixture, &[primary]);
        assert!(fixture
            .send_controller(vec![
                fixture.prepare_position_instruction(),
                fixture.open_cash_instruction(primary, None, 1, 1, 1),
            ])
            .is_err());
        assert_eq!(cash_open_snapshot(&fixture, &[primary]), before);
        assert!(fixture.svm.get_account(&fixture.position).is_none());
    }
}

#[test]
fn cash_open_hostile_accounts_missing_signer_and_replay_fail_without_mutation() {
    for case in 0u8..9 {
        let mut fixture = Fixture::new(170 + case);
        bootstrap_cash_open(&mut fixture, false);
        let primary =
            fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
        let mut open =
            fixture.open_cash_instruction(primary, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100);
        match case {
            0 => open.accounts[4] = AccountMeta::new(Pubkey::new_unique(), false),
            1 => open.accounts[5] = AccountMeta::new_readonly(fixture.market, false),
            2 => open.accounts[4] = AccountMeta::new(fixture.controller_risk, false),
            3 => open.accounts.swap(1, 2),
            4 => {
                open.accounts.pop();
            }
            5 => open.accounts[1] = AccountMeta::new_readonly(fixture.pool, false),
            6 => open
                .accounts
                .push(AccountMeta::new_readonly(Pubkey::new_unique(), false)),
            7 => open.accounts[6] = AccountMeta::new(fixture.vault, false),
            8 => open.accounts[7] = AccountMeta::new(primary, false),
            _ => unreachable!(),
        }
        let before = cash_open_snapshot(&fixture, &[primary]);
        assert!(fixture
            .send_controller(vec![fixture.prepare_position_instruction(), open])
            .is_err());
        assert_eq!(cash_open_snapshot(&fixture, &[primary]), before);
        assert!(fixture.svm.get_account(&fixture.position).is_none());
    }

    let mut writable_secondary = Fixture::new(179);
    bootstrap_cash_open(&mut writable_secondary, true);
    let primary =
        writable_secondary.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let secondary = writable_secondary.install_switchboard_price(
        &SECONDARY_FEED_ID,
        101_000_000,
        100_000,
        ORACLE_TIME,
    );
    let mut open = writable_secondary.open_cash_instruction(
        primary,
        Some(secondary),
        OPEN_COLLATERAL,
        OPEN_NOTIONAL,
        100,
    );
    open.accounts[8] = AccountMeta::new(secondary, false);
    let before = cash_open_snapshot(&writable_secondary, &[primary, secondary]);
    assert!(writable_secondary
        .send_controller(vec![
            writable_secondary.prepare_position_instruction(),
            open,
        ])
        .is_err());
    assert_eq!(
        cash_open_snapshot(&writable_secondary, &[primary, secondary]),
        before
    );
    assert!(writable_secondary
        .svm
        .get_account(&writable_secondary.position)
        .is_none());

    let mut missing_signer = Fixture::new(177);
    bootstrap_cash_open(&mut missing_signer, false);
    let primary =
        missing_signer.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    missing_signer
        .send_controller(vec![missing_signer.prepare_position_instruction()])
        .expect("prepare canonical position");
    let mut unsigned_open =
        missing_signer.open_cash_instruction(primary, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100);
    unsigned_open.accounts[0] =
        AccountMeta::new_readonly(missing_signer.controller.pubkey(), false);
    let before = cash_open_snapshot(&missing_signer, &[primary]);
    assert!(missing_signer.send(vec![unsigned_open], false).is_err());
    assert_eq!(cash_open_snapshot(&missing_signer, &[primary]), before);

    let mut replay = Fixture::new(178);
    bootstrap_cash_open(&mut replay, false);
    let primary = replay.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    replay
        .send_controller(vec![
            replay.prepare_position_instruction(),
            replay.open_cash_instruction(primary, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("open canonical position once");
    let before = cash_open_snapshot(&replay, &[primary]);
    assert!(replay
        .send_controller(vec![replay.open_cash_instruction(
            primary,
            None,
            OPEN_COLLATERAL,
            OPEN_NOTIONAL,
            100,
        )])
        .is_err());
    assert_eq!(cash_open_snapshot(&replay, &[primary]), before);
}

#[test]
fn deposits_capture_unsolicited_surplus_and_reject_custody_shortfalls_atomically() {
    let mut fixture = Fixture::new(123);
    fixture.send_bundle().expect("initialize canonical pool");
    fixture
        .send_controller(vec![
            fixture.controller_instruction(true),
            fixture.controller_instruction(false),
        ])
        .expect("register controller");
    fixture
        .send(vec![fixture.vault_instruction()], true)
        .expect("initialize canonical vault");

    const DONATION: u64 = 50_000_000;
    let mut vault_account = fixture.svm.get_account(&fixture.vault).unwrap();
    let mut vault_state = TokenAccount::unpack(&vault_account.data).unwrap();
    vault_state.amount = DONATION;
    TokenAccount::pack(vault_state, &mut vault_account.data).unwrap();
    fixture
        .svm
        .set_account(fixture.vault, vault_account)
        .unwrap();

    fixture
        .send_controller(vec![fixture.deposit_instruction(DEPOSIT_AMOUNT)])
        .expect("deposit reconciles unsolicited vault surplus");
    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    assert_eq!(pool.unallocated_vault_surplus.get(), u128::from(DONATION));
    assert_eq!(pool.idle_cash_collateral.get(), u128::from(DEPOSIT_AMOUNT));
    assert_eq!(pool.free_cash.get(), 0);
    assert_eq!(pool.protected_principal.get(), 0);

    let mut vault_account = fixture.svm.get_account(&fixture.vault).unwrap();
    let mut vault_state = TokenAccount::unpack(&vault_account.data).unwrap();
    vault_state.amount -= 1;
    TokenAccount::pack(vault_state, &mut vault_account.data).unwrap();
    fixture
        .svm
        .set_account(fixture.vault, vault_account)
        .unwrap();
    let pool_before = fixture.svm.get_account(&fixture.pool).unwrap();
    let controller_before = fixture.svm.get_account(&fixture.controller_risk).unwrap();
    let source_before = fixture.svm.get_account(&fixture.source).unwrap();
    let vault_before = fixture.svm.get_account(&fixture.vault).unwrap();
    assert!(fixture
        .send_controller(vec![fixture.deposit_instruction(1)])
        .is_err());
    assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), pool_before);
    assert_eq!(
        fixture.svm.get_account(&fixture.controller_risk).unwrap(),
        controller_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.source).unwrap(),
        source_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.vault).unwrap(),
        vault_before
    );
}

#[test]
fn hostile_deposit_shapes_and_failed_token_cpi_leave_custody_unchanged() {
    let mut fixture = Fixture::new(124);
    fixture.send_bundle().expect("initialize canonical pool");
    fixture
        .send_controller(vec![
            fixture.controller_instruction(true),
            fixture.controller_instruction(false),
        ])
        .expect("register controller");
    fixture
        .send(vec![fixture.vault_instruction()], true)
        .expect("initialize canonical vault");
    let before = custody_snapshot(&fixture);

    let mut missing_signature = fixture.deposit_metas();
    missing_signature[0] = AccountMeta::new_readonly(fixture.controller.pubkey(), false);
    let instruction = fixture.instruction(
        MartingalerInstruction::DepositCash { amount: 1 },
        missing_signature,
    );
    assert!(fixture.send(vec![instruction], false).is_err());
    assert_eq!(custody_snapshot(&fixture), before);

    let mut writable_authority = fixture.deposit_metas();
    writable_authority[0] = AccountMeta::new(fixture.controller.pubkey(), true);
    let instruction = fixture.instruction(
        MartingalerInstruction::DepositCash { amount: 1 },
        writable_authority,
    );
    assert!(fixture.send_controller(vec![instruction]).is_err());
    assert_eq!(custody_snapshot(&fixture), before);

    let mut wrong_token_program = fixture.deposit_metas();
    wrong_token_program[6] = AccountMeta::new_readonly(system_program::ID, false);
    let instruction = fixture.instruction(
        MartingalerInstruction::DepositCash { amount: 1 },
        wrong_token_program,
    );
    assert!(fixture.send_controller(vec![instruction]).is_err());
    assert_eq!(custody_snapshot(&fixture), before);

    let mut swapped_source_and_vault = fixture.deposit_metas();
    swapped_source_and_vault.swap(3, 4);
    let instruction = fixture.instruction(
        MartingalerInstruction::DepositCash { amount: 1 },
        swapped_source_and_vault,
    );
    assert!(fixture.send_controller(vec![instruction]).is_err());
    assert_eq!(custody_snapshot(&fixture), before);

    let mut extra_account = fixture.deposit_metas();
    extra_account.push(AccountMeta::new_readonly(system_program::ID, false));
    let instruction = fixture.instruction(
        MartingalerInstruction::DepositCash { amount: 1 },
        extra_account,
    );
    assert!(fixture.send_controller(vec![instruction]).is_err());
    assert_eq!(custody_snapshot(&fixture), before);

    assert!(fixture
        .send_controller(vec![fixture.deposit_instruction(SOURCE_BALANCE + 1)])
        .is_err());
    assert_eq!(custody_snapshot(&fixture), before);
}

#[test]
fn malformed_source_vault_and_controller_state_fail_before_token_movement() {
    let mut fixture = Fixture::new(127);
    fixture.send_bundle().expect("initialize canonical pool");
    fixture
        .send_controller(vec![
            fixture.controller_instruction(true),
            fixture.controller_instruction(false),
        ])
        .expect("register controller");
    fixture
        .send(vec![fixture.vault_instruction()], true)
        .expect("initialize canonical vault");

    let valid_source = fixture.svm.get_account(&fixture.source).unwrap();
    let valid_vault = fixture.svm.get_account(&fixture.vault).unwrap();
    let valid_controller = fixture.svm.get_account(&fixture.controller_risk).unwrap();
    let pool_before = fixture.svm.get_account(&fixture.pool).unwrap();

    for malformed_source in [
        {
            let mut account = valid_source.clone();
            let mut state = TokenAccount::unpack(&account.data).unwrap();
            state.owner = fixture.admin.pubkey();
            TokenAccount::pack(state, &mut account.data).unwrap();
            account
        },
        {
            let mut account = valid_source.clone();
            let mut state = TokenAccount::unpack(&account.data).unwrap();
            state.state = AccountState::Frozen;
            TokenAccount::pack(state, &mut account.data).unwrap();
            account
        },
        {
            let mut account = valid_source.clone();
            let mut state = TokenAccount::unpack(&account.data).unwrap();
            state.delegate = COption::Some(fixture.admin.pubkey());
            state.delegated_amount = 1;
            TokenAccount::pack(state, &mut account.data).unwrap();
            account
        },
        {
            let mut account = valid_source.clone();
            let mut state = TokenAccount::unpack(&account.data).unwrap();
            state.mint = Pubkey::new_unique();
            TokenAccount::pack(state, &mut account.data).unwrap();
            account
        },
        {
            let mut account = valid_source.clone();
            account.owner = system_program::ID;
            account
        },
        {
            let mut account = valid_source.clone();
            account.data.pop();
            account
        },
    ] {
        fixture
            .svm
            .set_account(fixture.source, malformed_source.clone())
            .unwrap();
        assert!(fixture
            .send_controller(vec![fixture.deposit_instruction(1)])
            .is_err());
        assert_eq!(
            fixture.svm.get_account(&fixture.source).unwrap(),
            malformed_source
        );
        assert_eq!(
            fixture.svm.get_account(&fixture.vault).unwrap(),
            valid_vault
        );
        assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), pool_before);
        assert_eq!(
            fixture.svm.get_account(&fixture.controller_risk).unwrap(),
            valid_controller
        );
        fixture
            .svm
            .set_account(fixture.source, valid_source.clone())
            .unwrap();
    }

    let mut malformed_vault = valid_vault.clone();
    let mut vault_state = TokenAccount::unpack(&malformed_vault.data).unwrap();
    vault_state.owner = fixture.admin.pubkey();
    TokenAccount::pack(vault_state, &mut malformed_vault.data).unwrap();
    fixture
        .svm
        .set_account(fixture.vault, malformed_vault.clone())
        .unwrap();
    assert!(fixture
        .send_controller(vec![fixture.deposit_instruction(1)])
        .is_err());
    assert_eq!(
        fixture.svm.get_account(&fixture.vault).unwrap(),
        malformed_vault
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.source).unwrap(),
        valid_source
    );
    assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), pool_before);
    fixture
        .svm
        .set_account(fixture.vault, valid_vault.clone())
        .unwrap();

    let mut malformed_controller = valid_controller.clone();
    malformed_controller.data[11] ^= 1;
    fixture
        .svm
        .set_account(fixture.controller_risk, malformed_controller.clone())
        .unwrap();
    assert!(fixture
        .send_controller(vec![fixture.deposit_instruction(1)])
        .is_err());
    assert_eq!(
        fixture.svm.get_account(&fixture.controller_risk).unwrap(),
        malformed_controller
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.source).unwrap(),
        valid_source
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.vault).unwrap(),
        valid_vault
    );
    assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), pool_before);
}

#[test]
fn two_controller_deposits_remain_isolated_and_aggregate_exactly() {
    let mut fixture = Fixture::new(125);
    fixture.send_bundle().expect("initialize canonical pool");
    fixture
        .send_controller(vec![
            fixture.controller_instruction(true),
            fixture.controller_instruction(false),
        ])
        .expect("register first controller");
    fixture
        .send(vec![fixture.vault_instruction()], true)
        .expect("initialize canonical vault");

    let second = deterministic_keypair(125, 8);
    let second_source = deterministic_keypair(125, 9).pubkey();
    let (second_root, _) = derive_controller(&id(), &fixture.pool, &second.pubkey());
    fixture
        .svm
        .airdrop(&second.pubkey(), 10_000_000_000)
        .expect("fund second controller");
    let mut second_source_data = vec![0u8; TokenAccount::LEN];
    TokenAccount::pack(
        TokenAccount {
            mint: fixture.quote_mint,
            owner: second.pubkey(),
            amount: SOURCE_BALANCE,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        },
        &mut second_source_data,
    )
    .unwrap();
    fixture
        .svm
        .set_account(
            second_source,
            Account {
                lamports: Rent::default().minimum_balance(TokenAccount::LEN),
                data: second_source_data,
                owner: spl_token::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();

    let registration_metas = vec![
        AccountMeta::new(second.pubkey(), true),
        AccountMeta::new_readonly(fixture.pool, false),
        AccountMeta::new(second_root, false),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    let registration = vec![
        fixture.instruction(
            MartingalerInstruction::PrepareController,
            registration_metas.clone(),
        ),
        fixture.instruction(
            MartingalerInstruction::InitializeController,
            registration_metas,
        ),
    ];
    let mut transaction_instructions = vec![
        ComputeBudgetInstruction::request_heap_frame(128 * 1024),
        ComputeBudgetInstruction::set_compute_unit_limit(TX_CU_LIMIT),
    ];
    transaction_instructions.extend(registration);
    let tx = Transaction::new_signed_with_payer(
        &transaction_instructions,
        Some(&fixture.payer.pubkey()),
        &[&fixture.payer, &second],
        fixture.svm.latest_blockhash(),
    );
    fixture
        .svm
        .send_transaction(tx)
        .expect("register second controller");

    const FIRST_DEPOSIT: u64 = 300_000_000;
    const SECOND_DEPOSIT: u64 = 175_000_000;
    fixture
        .send_controller(vec![fixture.deposit_instruction(FIRST_DEPOSIT)])
        .expect("deposit first controller cash");
    let second_deposit_metas = vec![
        AccountMeta::new_readonly(second.pubkey(), true),
        AccountMeta::new(fixture.pool, false),
        AccountMeta::new(second_root, false),
        AccountMeta::new(second_source, false),
        AccountMeta::new(fixture.vault, false),
        AccountMeta::new_readonly(fixture.quote_mint, false),
        AccountMeta::new_readonly(spl_token::ID, false),
    ];
    let second_deposit = fixture.instruction(
        MartingalerInstruction::DepositCash {
            amount: SECOND_DEPOSIT,
        },
        second_deposit_metas,
    );
    let transaction_instructions = vec![
        ComputeBudgetInstruction::request_heap_frame(128 * 1024),
        ComputeBudgetInstruction::set_compute_unit_limit(TX_CU_LIMIT),
        second_deposit,
    ];
    let tx = Transaction::new_signed_with_payer(
        &transaction_instructions,
        Some(&fixture.payer.pubkey()),
        &[&fixture.payer, &second],
        fixture.svm.latest_blockhash(),
    );
    fixture
        .svm
        .send_transaction(tx)
        .expect("deposit second controller cash");

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let first = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let second_state =
        decode_account::<ControllerRiskState>(&fixture.svm.get_account(&second_root).unwrap().data)
            .unwrap();
    let vault =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data).unwrap();
    assert_eq!(
        first.available_cash_collateral.get(),
        u128::from(FIRST_DEPOSIT)
    );
    assert_eq!(
        second_state.available_cash_collateral.get(),
        u128::from(SECOND_DEPOSIT)
    );
    assert_eq!(
        pool.idle_cash_collateral.get(),
        u128::from(FIRST_DEPOSIT + SECOND_DEPOSIT)
    );
    assert_eq!(
        vault.amount,
        FIRST_DEPOSIT.checked_add(SECOND_DEPOSIT).unwrap()
    );
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault.amount)
    );
}

#[test]
fn controller_close_refunds_exact_principal_in_close_only_and_rejects_replay() {
    let mut fixture = Fixture::new(180);
    bootstrap_cash_open(&mut fixture, false);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("open controller cash position");

    for account_key in [fixture.pool, fixture.market] {
        let mut account = fixture.svm.get_account(&account_key).unwrap();
        if account_key == fixture.pool {
            let mut state = decode_account::<PoolState>(&account.data).unwrap();
            state.status = PoolStatus::CloseOnly as u8;
            overwrite_account(&mut account.data, &state).unwrap();
        } else {
            let mut state = decode_account::<MarketRiskState>(&account.data).unwrap();
            state.status = PoolStatus::CloseOnly as u8;
            overwrite_account(&mut account.data, &state).unwrap();
        }
        fixture.svm.set_account(account_key, account).unwrap();
    }

    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 99_500_000, 100_000, ORACLE_TIME);
    let result = fixture
        .send_controller(vec![fixture.close_cash_instruction(exit)])
        .expect("close adverse cash position");
    eprintln!(
        "martingaler_phase2b5_nonpositive_close_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let market =
        decode_account::<MarketRiskState>(&fixture.svm.get_account(&fixture.market).unwrap().data)
            .unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let position =
        decode_account::<PositionState>(&fixture.svm.get_account(&fixture.position).unwrap().data)
            .unwrap();
    let vault =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data).unwrap();
    let destination =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.source).unwrap().data).unwrap();
    assert_eq!(vault.amount, DEPOSIT_AMOUNT - 5_000_000);
    assert_eq!(
        destination.amount,
        SOURCE_BALANCE - DEPOSIT_AMOUNT + 5_000_000
    );
    assert_eq!(pool.idle_cash_collateral.get(), 240_000_000);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.free_cash.get(), 5_000_000);
    assert_eq!(pool.reserved_open_profit.get(), 0);
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault.amount)
    );
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    assert_eq!(controller.available_cash_collateral.get(), 240_000_000);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(position.reserved_profit.get(), 0);
    assert_eq!(position.entry_price_e6.get(), 100_000_000);

    let before_replay = cash_open_snapshot(&fixture, &[exit]);
    assert!(fixture
        .send_controller(vec![fixture.close_cash_instruction(exit)])
        .is_err());
    assert_eq!(cash_open_snapshot(&fixture, &[exit]), before_replay);
}

#[test]
fn flat_controller_close_refunds_full_principal_and_rejects_replay() {
    let mut fixture = Fixture::new(200);
    bootstrap_cash_open(&mut fixture, false);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("open controller cash position");

    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let source_before = fixture.svm.get_account(&fixture.source).unwrap();
    let vault_before = fixture.svm.get_account(&fixture.vault).unwrap();
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();
    let source_amount_before = TokenAccount::unpack(&source_before.data).unwrap().amount;
    let vault_amount_before = TokenAccount::unpack(&vault_before.data).unwrap().amount;
    let result = fixture
        .send_controller(vec![fixture.close_cash_instruction(exit)])
        .expect("close flat cash position");
    eprintln!(
        "martingaler_phase2b5_flat_close_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let market =
        decode_account::<MarketRiskState>(&fixture.svm.get_account(&fixture.market).unwrap().data)
            .unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let position =
        decode_account::<PositionState>(&fixture.svm.get_account(&fixture.position).unwrap().data)
            .unwrap();
    let vault_after = fixture.svm.get_account(&fixture.vault).unwrap();
    let source_after = fixture.svm.get_account(&fixture.source).unwrap();
    let vault_amount_after = TokenAccount::unpack(&vault_after.data).unwrap().amount;
    let source_amount_after = TokenAccount::unpack(&source_after.data).unwrap().amount;
    assert_eq!(vault_amount_before - vault_amount_after, OPEN_COLLATERAL);
    assert_eq!(source_amount_after - source_amount_before, OPEN_COLLATERAL);
    assert_eq!(vault_amount_after, DEPOSIT_AMOUNT - OPEN_COLLATERAL);
    assert_eq!(
        source_amount_after,
        SOURCE_BALANCE - DEPOSIT_AMOUNT + OPEN_COLLATERAL
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );
    assert_eq!(pool.idle_cash_collateral.get(), 240_000_000);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.free_cash.get(), 0);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.residual_bad_debt.get(), 0);
    assert_eq!(pool.reserved_open_profit.get(), 0);
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault_amount_after)
    );
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    assert_eq!(controller.available_cash_collateral.get(), 240_000_000);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(position.reserved_profit.get(), 0);
    assert_eq!(position.entry_price_e6.get(), 100_000_000);

    fixture.svm.expire_blockhash();
    let before_replay = cash_open_snapshot(&fixture, &[exit]);
    assert!(fixture
        .send_controller(vec![fixture.close_cash_instruction(exit)])
        .is_err());
    assert_eq!(cash_open_snapshot(&fixture, &[exit]), before_replay);
}

#[test]
fn claim_and_queue_page_preparation_are_isolated_inert_dust_tolerant_and_single_use() {
    let mut fixture = opened_profitable_fixture(230, 100_000_000);
    fixture.svm.airdrop(&fixture.claim(), 1).unwrap();
    let pool_before = fixture.svm.get_account(&fixture.pool).unwrap();
    let position_before = fixture.svm.get_account(&fixture.position).unwrap();
    fixture
        .send_controller(vec![fixture.prepare_claim_instruction()])
        .expect("prepare dust-prefunded claim in isolation");
    let claim = fixture.svm.get_account(&fixture.claim()).unwrap();
    assert_eq!(claim.owner, id());
    assert_eq!(claim.data.len(), account_len::<ClaimState>());
    assert!(claim.data.iter().all(|byte| *byte == 0));
    assert!(claim.lamports >= Rent::default().minimum_balance(claim.data.len()));
    assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), pool_before);
    assert_eq!(
        fixture.svm.get_account(&fixture.position).unwrap(),
        position_before
    );
    fixture.svm.expire_blockhash();
    let before_replay = account_snapshot(
        &fixture,
        &[
            fixture.controller.pubkey(),
            fixture.position,
            fixture.claim(),
        ],
    );
    assert!(fixture
        .send_controller(vec![fixture.prepare_claim_instruction()])
        .is_err());
    assert_eq!(
        account_snapshot(
            &fixture,
            &[
                fixture.controller.pubkey(),
                fixture.position,
                fixture.claim()
            ]
        ),
        before_replay
    );

    fixture.svm.airdrop(&fixture.queue_page(1), 2).unwrap();
    let pool_before = fixture.svm.get_account(&fixture.pool).unwrap();
    fixture
        .send_controller(vec![fixture.prepare_queue_page_instruction(1)])
        .expect("prepare dust-prefunded queue page in isolation");
    let page = fixture.svm.get_account(&fixture.queue_page(1)).unwrap();
    assert_eq!(page.owner, id());
    assert_eq!(page.data.len(), account_len::<QueuePageState>());
    assert!(page.data.iter().all(|byte| *byte == 0));
    assert!(page.lamports >= Rent::default().minimum_balance(page.data.len()));
    assert_eq!(fixture.svm.get_account(&fixture.pool).unwrap(), pool_before);
    fixture.svm.expire_blockhash();
    let before_replay = account_snapshot(
        &fixture,
        &[
            fixture.controller.pubkey(),
            fixture.pool,
            fixture.queue_page(1),
        ],
    );
    assert!(fixture
        .send_controller(vec![fixture.prepare_queue_page_instruction(1)])
        .is_err());
    assert_eq!(
        account_snapshot(
            &fixture,
            &[
                fixture.controller.pubkey(),
                fixture.pool,
                fixture.queue_page(1)
            ]
        ),
        before_replay
    );
}

#[test]
fn prepare_claim_rejects_account_abi_pda_privilege_and_position_boundary_matrix_atomically() {
    for case in 0u8..16 {
        let mut fixture = opened_profitable_fixture(231u8.wrapping_add(case), 100_000_000);
        let mut metas = fixture.prepare_claim_metas();
        match case {
            0 => {
                metas.pop();
            }
            1 => metas.push(AccountMeta::new_readonly(Pubkey::new_unique(), false)),
            2 => metas[0].is_writable = false,
            3 => metas[0].is_signer = false,
            4 => metas[1].is_writable = false,
            5 => {
                metas[1].pubkey = fixture.controller.pubkey();
                metas[1].is_signer = true;
            }
            6 => metas[2].is_writable = false,
            7 => {
                metas[2].pubkey = fixture.controller.pubkey();
                metas[2].is_signer = true;
            }
            8 => metas[2].pubkey = Pubkey::new_unique(),
            9 => metas[2].pubkey = fixture.position,
            10 => metas[3].pubkey = spl_token::ID,
            11 => {
                let mut account = fixture.svm.get_account(&fixture.position).unwrap();
                account.data[12] ^= 1;
                fixture.svm.set_account(fixture.position, account).unwrap();
            }
            12 => {
                let mut account = fixture.svm.get_account(&fixture.position).unwrap();
                let mut state = decode_account::<PositionState>(&account.data).unwrap();
                state.controller = Key32::new([0xD1; 32]);
                overwrite_account(&mut account.data, &state).unwrap();
                fixture.svm.set_account(fixture.position, account).unwrap();
            }
            13 => {
                let mut account = fixture.svm.get_account(&fixture.position).unwrap();
                account.owner = system_program::ID;
                fixture.svm.set_account(fixture.position, account).unwrap();
            }
            14 => {
                let mut account = fixture.svm.get_account(&fixture.position).unwrap();
                account.data.pop();
                fixture.svm.set_account(fixture.position, account).unwrap();
            }
            15 => {
                let mut account = fixture.svm.get_account(&fixture.position).unwrap();
                account.lamports = 1;
                fixture.svm.set_account(fixture.position, account).unwrap();
            }
            _ => unreachable!(),
        }
        let instruction = fixture.instruction(
            MartingalerInstruction::PrepareClaim {
                position_id: POSITION_ID,
            },
            metas,
        );
        let before = account_snapshot(
            &fixture,
            &[
                fixture.controller.pubkey(),
                fixture.position,
                fixture.claim(),
            ],
        );
        let result = if case == 3 {
            fixture.send(vec![instruction], false)
        } else {
            fixture.send_controller(vec![instruction])
        };
        assert!(result.is_err(), "case {case}");
        assert_eq!(
            account_snapshot(
                &fixture,
                &[
                    fixture.controller.pubkey(),
                    fixture.position,
                    fixture.claim()
                ]
            ),
            before,
            "case {case}"
        );
    }
}

#[test]
fn prepare_queue_page_rejects_account_abi_pda_privilege_and_pool_boundary_matrix_atomically() {
    for case in 0u8..18 {
        let mut fixture = opened_profitable_fixture(247u8.wrapping_add(case), 100_000_000);
        let page_index = if case == 11 { 0 } else { 1 };
        let next_page = fixture.queue_page(1);
        let mut metas = fixture.prepare_queue_page_metas(page_index);
        match case {
            0 => {
                metas.pop();
            }
            1 => metas.push(AccountMeta::new_readonly(Pubkey::new_unique(), false)),
            2 => metas[0].is_writable = false,
            3 => metas[0].is_signer = false,
            4 => metas[1].is_writable = false,
            5 => {
                metas[1].pubkey = fixture.controller.pubkey();
                metas[1].is_signer = true;
            }
            6 => metas[2].is_writable = false,
            7 => {
                metas[2].pubkey = fixture.controller.pubkey();
                metas[2].is_signer = true;
            }
            8 => metas[2].pubkey = Pubkey::new_unique(),
            9 => metas[2].pubkey = fixture.pool,
            10 => metas[3].pubkey = spl_token::ID,
            11 => {}
            12 => {
                let mut account = fixture.svm.get_account(&fixture.pool).unwrap();
                account.data[12] ^= 1;
                fixture.svm.set_account(fixture.pool, account).unwrap();
            }
            13 => {
                let mut account = fixture.svm.get_account(&fixture.pool).unwrap();
                account.owner = system_program::ID;
                fixture.svm.set_account(fixture.pool, account).unwrap();
            }
            14 => {
                let mut account = fixture.svm.get_account(&fixture.pool).unwrap();
                account.data.pop();
                fixture.svm.set_account(fixture.pool, account).unwrap();
            }
            15 => {
                let mut account = fixture.svm.get_account(&fixture.pool).unwrap();
                account.lamports = 1;
                fixture.svm.set_account(fixture.pool, account).unwrap();
            }
            16 => replace_account(&mut fixture, next_page, spl_token::ID, 0, true),
            17 => replace_account(&mut fixture, next_page, system_program::ID, 1, true),
            _ => unreachable!(),
        }
        let instruction = fixture.instruction(
            MartingalerInstruction::PrepareQueuePage { page_index },
            metas,
        );
        let before = account_snapshot(
            &fixture,
            &[
                fixture.controller.pubkey(),
                fixture.pool,
                fixture.queue_page(1),
            ],
        );
        let result = if case == 3 {
            fixture.send(vec![instruction], false)
        } else {
            fixture.send_controller(vec![instruction])
        };
        assert!(result.is_err(), "case {case}");
        assert_eq!(
            account_snapshot(
                &fixture,
                &[
                    fixture.controller.pubkey(),
                    fixture.pool,
                    fixture.queue_page(1)
                ]
            ),
            before,
            "case {case}"
        );
    }
}

#[test]
fn profitable_cash_close_fully_pays_principal_and_profit_without_claim() {
    const PAYABLE_PROFIT: u64 = 8_820_000;
    let mut fixture = Fixture::new(206);
    bootstrap_cash_open(&mut fixture, false);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("open profitable cash position");
    let mut pool_account = fixture.svm.get_account(&fixture.pool).unwrap();
    let mut pool_state = decode_account::<PoolState>(&pool_account.data).unwrap();
    pool_state.status = PoolStatus::CloseOnly as u8;
    pool_state.idle_cash_collateral =
        U128Le::new(pool_state.idle_cash_collateral.get() - u128::from(PAYABLE_PROFIT));
    pool_state.free_cash = U128Le::new(u128::from(PAYABLE_PROFIT));
    overwrite_account(&mut pool_account.data, &pool_state).unwrap();
    fixture.svm.set_account(fixture.pool, pool_account).unwrap();
    let mut market_account = fixture.svm.get_account(&fixture.market).unwrap();
    let mut market_state = decode_account::<MarketRiskState>(&market_account.data).unwrap();
    market_state.status = PoolStatus::CloseOnly as u8;
    overwrite_account(&mut market_account.data, &market_state).unwrap();
    fixture
        .svm
        .set_account(fixture.market, market_account)
        .unwrap();

    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    let source_before =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.source).unwrap().data)
            .unwrap()
            .amount;
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();
    let result = fixture
        .send_controller(vec![
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::NoClaim)
        ])
        .expect("fully settle profitable cash close");
    eprintln!(
        "martingaler_phase2b6_fully_paid_close_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let market =
        decode_account::<MarketRiskState>(&fixture.svm.get_account(&fixture.market).unwrap().data)
            .unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let position =
        decode_account::<PositionState>(&fixture.svm.get_account(&fixture.position).unwrap().data)
            .unwrap();
    let vault =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data).unwrap();
    let destination =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.source).unwrap().data).unwrap();
    assert_eq!(
        destination.amount - source_before,
        OPEN_COLLATERAL + PAYABLE_PROFIT
    );
    assert_eq!(
        vault.amount,
        DEPOSIT_AMOUNT - OPEN_COLLATERAL - PAYABLE_PROFIT
    );
    assert_eq!(pool.free_cash.get(), 0);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.total_queued.get(), 0);
    assert_eq!(pool.reserved_open_profit.get(), 0);
    assert_eq!(pool.fee_accrual.get(), 180_000);
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault.amount)
    );
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.active_claims.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert!(fixture.svm.get_account(&fixture.claim()).is_none());
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );

    fixture.svm.expire_blockhash();
    let before_replay = cash_open_snapshot(&fixture, &[exit]);
    assert!(fixture
        .send_controller(vec![
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::NoClaim,)
        ])
        .is_err());
    assert_eq!(cash_open_snapshot(&fixture, &[exit]), before_replay);
}

#[test]
fn profitable_cash_close_partially_pays_and_atomically_appends_current_tail() {
    const FREE_CASH: u64 = 1_000_000;
    const QUEUED: u64 = 7_820_000;
    let mut fixture = Fixture::new(207);
    bootstrap_cash_open(&mut fixture, false);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("open cash position for partial profitable settlement");
    let mut pool_account = fixture.svm.get_account(&fixture.pool).unwrap();
    let mut pool_state = decode_account::<PoolState>(&pool_account.data).unwrap();
    pool_state.idle_cash_collateral =
        U128Le::new(pool_state.idle_cash_collateral.get() - u128::from(FREE_CASH));
    pool_state.free_cash = U128Le::new(u128::from(FREE_CASH));
    overwrite_account(&mut pool_account.data, &pool_state).unwrap();
    fixture.svm.set_account(fixture.pool, pool_account).unwrap();

    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    let result = fixture
        .send_controller(vec![
            fixture.prepare_claim_instruction(),
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::CurrentTail),
        ])
        .expect("prepare claim and partially settle in one transaction");
    eprintln!(
        "martingaler_phase2b6_partial_current_tail_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let claim =
        decode_account::<ClaimState>(&fixture.svm.get_account(&fixture.claim()).unwrap().data)
            .unwrap();
    let page = decode_account::<QueuePageState>(
        &fixture.svm.get_account(&fixture.page_zero).unwrap().data,
    )
    .unwrap();
    let vault =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data).unwrap();
    assert_eq!(vault.amount, DEPOSIT_AMOUNT - OPEN_COLLATERAL - FREE_CASH);
    assert_eq!(pool.total_queued.get(), u128::from(QUEUED));
    assert_eq!(pool.free_cash.get(), 0);
    assert_eq!(pool.fee_accrual.get(), 180_000);
    assert_eq!(claim.original_face.get(), u128::from(QUEUED));
    assert_eq!(claim.remaining_face.get(), u128::from(QUEUED));
    assert_eq!(claim.sequence.get(), 1);
    assert_eq!(claim.queue_page_index.get(), 0);
    assert_eq!(claim.queue_slot.get(), 0);
    assert_eq!(page.entries[0].claim.0, fixture.claim().to_bytes());
    assert_eq!(page.entries[0].remaining_face.get(), u128::from(QUEUED));
    assert_eq!(controller.queued_claim_face.get(), u128::from(QUEUED));
    assert_eq!(controller.active_claims.get(), 1);
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault.amount)
    );

    fixture.svm.expire_blockhash();
    let before_replay = cash_open_snapshot(&fixture, &[exit]);
    let claim_before = fixture.svm.get_account(&fixture.claim()).unwrap();
    assert!(fixture
        .send_controller(vec![
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::CurrentTail,)
        ])
        .is_err());
    assert_eq!(cash_open_snapshot(&fixture, &[exit]), before_replay);
    assert_eq!(
        fixture.svm.get_account(&fixture.claim()).unwrap(),
        claim_before
    );
}

#[test]
fn settled_fifo_head_blocks_profit_bypass_and_accepts_new_current_tail_claim() {
    const QUEUED: u64 = 8_820_000;
    let mut fixture = Fixture::new(208);
    bootstrap_cash_open(&mut fixture, false);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("open cash position behind settled queue head");

    let mut pool_account = fixture.svm.get_account(&fixture.pool).unwrap();
    let mut pool_state = decode_account::<PoolState>(&pool_account.data).unwrap();
    pool_state.next_claim_sequence = U128Le::new(2);
    pool_state.head_claim_sequence = U128Le::new(1);
    pool_state.tail_claim_sequence = U128Le::new(1);
    pool_state.queue_flags = QUEUE_FLAG_HEAD_SETTLED;
    overwrite_account(&mut pool_account.data, &pool_state).unwrap();
    fixture.svm.set_account(fixture.pool, pool_account).unwrap();
    let mut page_account = fixture.svm.get_account(&fixture.page_zero).unwrap();
    let mut page_state = decode_account::<QueuePageState>(&page_account.data).unwrap();
    page_state.entries[0] = QueueEntry {
        claim: Key32::new([0xA5; 32]),
        sequence: U128Le::new(1),
        remaining_face: U128Le::new(0),
    };
    page_state.len = U16Le::new(1);
    page_state.next_append_sequence = U128Le::new(2);
    overwrite_account(&mut page_account.data, &page_state).unwrap();
    fixture
        .svm
        .set_account(fixture.page_zero, page_account)
        .unwrap();

    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_claim_instruction(),
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::CurrentTail),
        ])
        .expect("append profit behind settled unpopped head");
    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let claim =
        decode_account::<ClaimState>(&fixture.svm.get_account(&fixture.claim()).unwrap().data)
            .unwrap();
    let vault =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data).unwrap();
    assert_eq!(vault.amount, DEPOSIT_AMOUNT - OPEN_COLLATERAL);
    assert_eq!(pool.free_cash.get(), 0);
    assert_eq!(pool.total_queued.get(), u128::from(QUEUED));
    assert_eq!(pool.head_claim_sequence.get(), 1);
    assert_eq!(pool.tail_claim_sequence.get(), 2);
    assert_ne!(pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED, 0);
    assert_eq!(claim.sequence.get(), 2);
    assert_eq!(claim.original_face.get(), u128::from(QUEUED));
}

#[test]
fn full_tail_rollover_bundle_is_atomic_bounded_and_initializes_current_sequence() {
    const QUEUED: u64 = 8_820_000;
    let mut fixture = Fixture::new(209);
    bootstrap_cash_open(&mut fixture, false);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("open cash position for queue rollover");

    let mut pool_account = fixture.svm.get_account(&fixture.pool).unwrap();
    let mut pool_state = decode_account::<PoolState>(&pool_account.data).unwrap();
    pool_state.total_queued = U128Le::new(QUEUE_PAGE_CAPACITY as u128);
    pool_state.next_claim_sequence = U128Le::new(QUEUE_PAGE_CAPACITY as u128 + 1);
    pool_state.head_claim_sequence = U128Le::new(1);
    pool_state.tail_claim_sequence = U128Le::new(QUEUE_PAGE_CAPACITY as u128);
    overwrite_account(&mut pool_account.data, &pool_state).unwrap();
    fixture.svm.set_account(fixture.pool, pool_account).unwrap();
    let mut page_account = fixture.svm.get_account(&fixture.page_zero).unwrap();
    let mut page_state = decode_account::<QueuePageState>(&page_account.data).unwrap();
    for (slot, entry) in page_state.entries.iter_mut().enumerate() {
        *entry = QueueEntry {
            claim: Key32::new([slot as u8 + 1; 32]),
            sequence: U128Le::new(slot as u128 + 1),
            remaining_face: U128Le::new(1),
        };
    }
    page_state.len = U16Le::new(QUEUE_PAGE_CAPACITY as u16);
    page_state.next_append_sequence = U128Le::new(QUEUE_PAGE_CAPACITY as u128 + 1);
    overwrite_account(&mut page_account.data, &page_state).unwrap();
    fixture
        .svm
        .set_account(fixture.page_zero, page_account)
        .unwrap();

    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    let instructions = vec![
        ComputeBudgetInstruction::request_heap_frame(128 * 1024),
        ComputeBudgetInstruction::set_compute_unit_limit(TX_CU_LIMIT),
        fixture.prepare_claim_instruction(),
        fixture.prepare_queue_page_instruction(1),
        fixture.profitable_close_instruction(exit, ProfitSettlementShape::Rollover),
    ];
    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&fixture.payer.pubkey()),
        &[&fixture.payer, &fixture.controller],
        fixture.svm.latest_blockhash(),
    );
    let wire_len = bincode::serialize(&tx).unwrap().len();
    assert!(wire_len <= 1_232, "rollover bundle wire length {wire_len}");
    let result = fixture
        .svm
        .send_transaction(tx)
        .expect("prepare claim/page and roll full tail atomically");
    eprintln!(
        "martingaler_phase2b6_rollover_bundle_compute_units={};wire_bytes={wire_len}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let old_page = decode_account::<QueuePageState>(
        &fixture.svm.get_account(&fixture.page_zero).unwrap().data,
    )
    .unwrap();
    let new_page = decode_account::<QueuePageState>(
        &fixture
            .svm
            .get_account(&fixture.queue_page(1))
            .unwrap()
            .data,
    )
    .unwrap();
    let claim =
        decode_account::<ClaimState>(&fixture.svm.get_account(&fixture.claim()).unwrap().data)
            .unwrap();
    assert_eq!(old_page.next_page_index.get(), 1);
    assert_eq!(new_page.previous_page_index.get(), 0);
    assert_eq!(new_page.entries[0].claim.0, fixture.claim().to_bytes());
    assert_eq!(new_page.entries[0].remaining_face.get(), u128::from(QUEUED));
    assert_eq!(claim.sequence.get(), 33);
    assert_eq!(claim.queue_page_index.get(), 1);
    assert_eq!(pool.tail_page_index.get(), 1);
    assert_eq!(pool.next_page_index.get(), 2);
    assert_eq!(pool.tail_claim_sequence.get(), 33);
    assert_eq!(
        pool.total_queued.get(),
        QUEUE_PAGE_CAPACITY as u128 + u128::from(QUEUED)
    );

    let mut rollback = Fixture::new(210);
    bootstrap_cash_open(&mut rollback, false);
    let entry = rollback.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    rollback
        .send_controller(vec![
            rollback.prepare_position_instruction(),
            rollback.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .unwrap();
    let mut pool_account = rollback.svm.get_account(&rollback.pool).unwrap();
    let mut pool_state = decode_account::<PoolState>(&pool_account.data).unwrap();
    pool_state.idle_cash_collateral =
        U128Le::new(pool_state.idle_cash_collateral.get() - 8_820_000);
    pool_state.free_cash = U128Le::new(8_820_000);
    overwrite_account(&mut pool_account.data, &pool_state).unwrap();
    rollback
        .svm
        .set_account(rollback.pool, pool_account)
        .unwrap();
    let exit = rollback.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    let payer_before = rollback.svm.get_account(&rollback.payer.pubkey()).unwrap();
    let before = cash_open_snapshot(&rollback, &[exit]);
    assert!(rollback
        .send_controller(vec![
            rollback.prepare_claim_instruction(),
            rollback.profitable_close_instruction(exit, ProfitSettlementShape::CurrentTail),
        ])
        .is_err());
    assert!(rollback.svm.get_account(&rollback.claim()).is_none());
    let payer_after = rollback.svm.get_account(&rollback.payer.pubkey()).unwrap();
    assert_eq!(payer_after.lamports + 10_000, payer_before.lamports);
    assert_eq!(payer_after.data, payer_before.data);
    assert_eq!(payer_after.owner, payer_before.owner);
    assert_eq!(cash_open_snapshot(&rollback, &[exit]), before);
}

#[test]
fn real_sbf_favorable_zero_atom_close_returns_only_principal_without_claim() {
    let mut fixture = Fixture::new(211);
    bootstrap_cash_open(&mut fixture, false);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 1, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_COLLATERAL, 1),
        ])
        .expect("open zero-atom profitable-close fixture");
    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_001, 1, ORACLE_TIME);
    let source_before =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.source).unwrap().data)
            .unwrap()
            .amount;
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();
    let result = fixture
        .send_controller(vec![
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::NoClaim)
        ])
        .expect("close positive-price-move position whose atom profit floors to zero");
    eprintln!(
        "martingaler_phase2b6_zero_atom_close_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let market =
        decode_account::<MarketRiskState>(&fixture.svm.get_account(&fixture.market).unwrap().data)
            .unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let position =
        decode_account::<PositionState>(&fixture.svm.get_account(&fixture.position).unwrap().data)
            .unwrap();
    let destination =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.source).unwrap().data).unwrap();
    assert_eq!(destination.amount - source_before, OPEN_COLLATERAL);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.reserved_open_profit.get(), 0);
    assert_eq!(pool.total_queued.get(), 0);
    assert_eq!(pool.fee_accrual.get(), 0);
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.active_claims.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert!(fixture.svm.get_account(&fixture.claim()).is_none());
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );
    fixture.svm.expire_blockhash();
    let before_replay = close_boundary_snapshot(&fixture, exit);
    assert!(fixture
        .send_controller(vec![
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::NoClaim),
        ])
        .is_err());
    assert_eq!(close_boundary_snapshot(&fixture, exit), before_replay);
}

#[test]
fn current_tail_profitable_close_rejects_claim_page_shape_and_preparation_matrix_atomically() {
    for case in 0u8..21 {
        let mut fixture = opened_profitable_fixture(40u8.wrapping_add(case), 100_000_000);
        let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 1, ORACLE_TIME);
        if case != 0 {
            fixture
                .send_controller(vec![fixture.prepare_claim_instruction()])
                .unwrap();
        }
        let mut instruction =
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::CurrentTail);
        match case {
            0 => {}
            1 => {
                let mut account = fixture.svm.get_account(&fixture.claim()).unwrap();
                account.owner = spl_token::ID;
                fixture.svm.set_account(fixture.claim(), account).unwrap();
            }
            2 => {
                let mut account = fixture.svm.get_account(&fixture.claim()).unwrap();
                account.data.pop();
                fixture.svm.set_account(fixture.claim(), account).unwrap();
            }
            3 => {
                let mut account = fixture.svm.get_account(&fixture.claim()).unwrap();
                account.lamports = 1;
                fixture.svm.set_account(fixture.claim(), account).unwrap();
            }
            4 => {
                let mut account = fixture.svm.get_account(&fixture.claim()).unwrap();
                let stale = ClaimState::new_cash(
                    Key32::new(fixture.pool.to_bytes()),
                    Key32::new(fixture.controller.pubkey().to_bytes()),
                    POSITION_ID,
                    MARKET_ID,
                    0,
                    0,
                    1,
                    1,
                )
                .unwrap();
                let bump = derive_claim(&id(), &fixture.position).1;
                initialize_account(&mut account.data, bump, &stale).unwrap();
                fixture.svm.set_account(fixture.claim(), account).unwrap();
            }
            5 => instruction.accounts[12].pubkey = Pubkey::new_unique(),
            6 => instruction.accounts[12].is_writable = false,
            7 => {
                instruction.accounts[12].pubkey = fixture.controller.pubkey();
                instruction.accounts[12].is_signer = true;
            }
            8 => instruction.accounts[12].pubkey = fixture.page_zero,
            9 => instruction.accounts[13].pubkey = fixture.queue_page(1),
            10 => instruction.accounts[13].is_writable = false,
            11 => {
                instruction.accounts[13].pubkey = fixture.controller.pubkey();
                instruction.accounts[13].is_signer = true;
            }
            12 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                account.owner = spl_token::ID;
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            13 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                account.data.pop();
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            14 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                account.lamports = 1;
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            15 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                account.data[12] ^= 1;
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            16 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                let mut page = decode_account::<QueuePageState>(&account.data).unwrap();
                page.pool = Key32::new([0xA6; 32]);
                overwrite_account(&mut account.data, &page).unwrap();
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            17 => {
                instruction.accounts.pop();
            }
            18 => instruction
                .accounts
                .push(AccountMeta::new_readonly(Pubkey::new_unique(), false)),
            19 => {
                let mut account = fixture.svm.get_account(&fixture.claim()).unwrap();
                account.data[0] = 1;
                fixture.svm.set_account(fixture.claim(), account).unwrap();
            }
            20 => {
                let mut account = fixture.svm.get_account(&fixture.claim()).unwrap();
                let stale = ClaimState::new_cash(
                    Key32::new(fixture.pool.to_bytes()),
                    Key32::new(fixture.controller.pubkey().to_bytes()),
                    POSITION_ID,
                    MARKET_ID,
                    0,
                    0,
                    1,
                    1,
                )
                .unwrap();
                let wrong_bump = derive_claim(&id(), &fixture.position).1 ^ 1;
                initialize_account(&mut account.data, wrong_bump, &stale).unwrap();
                fixture.svm.set_account(fixture.claim(), account).unwrap();
            }
            _ => unreachable!(),
        }
        let before = close_boundary_snapshot(&fixture, exit);
        assert!(
            fixture.send_controller(vec![instruction]).is_err(),
            "current-tail case {case}"
        );
        assert_eq!(
            close_boundary_snapshot(&fixture, exit),
            before,
            "current-tail case {case}"
        );
    }
}

#[test]
fn rollover_profitable_close_rejects_next_page_and_tail_relation_matrix_atomically() {
    for case in 0u8..17 {
        let mut fixture = opened_profitable_fixture(70u8.wrapping_add(case), 100_000_000);
        fill_current_tail(&mut fixture);
        let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 1, ORACLE_TIME);
        fixture
            .send_controller(vec![fixture.prepare_claim_instruction()])
            .unwrap();
        if case != 0 {
            fixture
                .send_controller(vec![fixture.prepare_queue_page_instruction(1)])
                .unwrap();
        }
        let mut instruction =
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::Rollover);
        match case {
            0 => {}
            1 => {
                let mut account = fixture.svm.get_account(&fixture.queue_page(1)).unwrap();
                account.owner = spl_token::ID;
                fixture
                    .svm
                    .set_account(fixture.queue_page(1), account)
                    .unwrap();
            }
            2 => {
                let mut account = fixture.svm.get_account(&fixture.queue_page(1)).unwrap();
                account.data.pop();
                fixture
                    .svm
                    .set_account(fixture.queue_page(1), account)
                    .unwrap();
            }
            3 => {
                let mut account = fixture.svm.get_account(&fixture.queue_page(1)).unwrap();
                account.lamports = 1;
                fixture
                    .svm
                    .set_account(fixture.queue_page(1), account)
                    .unwrap();
            }
            4 => {
                let mut account = fixture.svm.get_account(&fixture.queue_page(1)).unwrap();
                let stale = QueuePageState::new(
                    Key32::new(fixture.pool.to_bytes()),
                    1,
                    0,
                    QUEUE_PAGE_CAPACITY as u128 + 1,
                )
                .unwrap();
                let bump = derive_queue_page(&id(), &fixture.pool, 1).1;
                initialize_account(&mut account.data, bump, &stale).unwrap();
                fixture
                    .svm
                    .set_account(fixture.queue_page(1), account)
                    .unwrap();
            }
            5 => instruction.accounts[14].pubkey = Pubkey::new_unique(),
            6 => instruction.accounts[14].is_writable = false,
            7 => {
                instruction.accounts[14].pubkey = fixture.controller.pubkey();
                instruction.accounts[14].is_signer = true;
            }
            8 => instruction.accounts[14].pubkey = fixture.page_zero,
            9 => instruction.accounts[13].pubkey = fixture.queue_page(1),
            10 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                account.data[12] ^= 1;
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            11 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                let mut page = decode_account::<QueuePageState>(&account.data).unwrap();
                page.pool = Key32::new([0xB7; 32]);
                overwrite_account(&mut account.data, &page).unwrap();
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            12 => {
                let mut account = fixture.svm.get_account(&fixture.pool).unwrap();
                let mut pool = decode_account::<PoolState>(&account.data).unwrap();
                pool.next_page_index = percolator_prog::martingaler_state::U64Le::new(2);
                overwrite_account(&mut account.data, &pool).unwrap();
                fixture.svm.set_account(fixture.pool, account).unwrap();
            }
            13 => {
                instruction.accounts.pop();
            }
            14 => instruction
                .accounts
                .push(AccountMeta::new_readonly(Pubkey::new_unique(), false)),
            15 => {
                let mut account = fixture.svm.get_account(&fixture.queue_page(1)).unwrap();
                account.data[0] = 1;
                fixture
                    .svm
                    .set_account(fixture.queue_page(1), account)
                    .unwrap();
            }
            16 => {
                let mut account = fixture.svm.get_account(&fixture.queue_page(1)).unwrap();
                let stale = QueuePageState::new(
                    Key32::new(fixture.pool.to_bytes()),
                    1,
                    0,
                    QUEUE_PAGE_CAPACITY as u128 + 1,
                )
                .unwrap();
                let wrong_bump = derive_queue_page(&id(), &fixture.pool, 1).1 ^ 1;
                initialize_account(&mut account.data, wrong_bump, &stale).unwrap();
                fixture
                    .svm
                    .set_account(fixture.queue_page(1), account)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = close_boundary_snapshot(&fixture, exit);
        assert!(
            fixture.send_controller(vec![instruction]).is_err(),
            "rollover case {case}"
        );
        assert_eq!(
            close_boundary_snapshot(&fixture, exit),
            before,
            "rollover case {case}"
        );
    }
}

#[test]
fn profitable_close_hostile_authority_accounts_oracle_custody_and_modes_are_atomic() {
    for case in 0u8..8 {
        let mut fixture = Fixture::new(220 + case);
        bootstrap_cash_open(&mut fixture, false);
        let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
        fixture
            .send_controller(vec![
                fixture.prepare_position_instruction(),
                fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
            ])
            .unwrap();
        let exit = match case {
            6 => {
                fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME - 31)
            }
            7 => fixture.install_pyth_price(&[0x77; 32], 101_000_000, 100_000, ORACLE_TIME),
            _ => fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME),
        };
        let mut instruction =
            fixture.profitable_close_instruction(exit, ProfitSettlementShape::NoClaim);
        match case {
            0 => instruction.accounts[0].is_signer = false,
            1 => instruction.accounts[11].is_writable = true,
            2 => instruction.accounts[5].is_writable = true,
            3 => instruction.accounts[8].pubkey = fixture.vault,
            4 => {
                let mut account = fixture.svm.get_account(&fixture.source).unwrap();
                let mut token = TokenAccount::unpack(&account.data).unwrap();
                token.owner = fixture.admin.pubkey();
                TokenAccount::pack(token, &mut account.data).unwrap();
                fixture.svm.set_account(fixture.source, account).unwrap();
            }
            5 => {
                for key in [fixture.pool, fixture.market] {
                    let mut account = fixture.svm.get_account(&key).unwrap();
                    if key == fixture.pool {
                        let mut state = decode_account::<PoolState>(&account.data).unwrap();
                        state.status = PoolStatus::ClaimsOnly as u8;
                        overwrite_account(&mut account.data, &state).unwrap();
                    } else {
                        let mut state = decode_account::<MarketRiskState>(&account.data).unwrap();
                        state.status = PoolStatus::ClaimsOnly as u8;
                        overwrite_account(&mut account.data, &state).unwrap();
                    }
                    fixture.svm.set_account(key, account).unwrap();
                }
            }
            6 | 7 => {}
            _ => unreachable!(),
        }
        let before = cash_open_snapshot(&fixture, &[exit]);
        let result = if case == 0 {
            fixture.send(vec![instruction], false)
        } else {
            fixture.send_controller(vec![instruction])
        };
        assert!(result.is_err(), "hostile profitable-close case {case}");
        assert_eq!(cash_open_snapshot(&fixture, &[exit]), before);
        assert!(fixture.svm.get_account(&fixture.claim()).is_none());
    }

    let mut shortfall = Fixture::new(229);
    bootstrap_cash_open(&mut shortfall, false);
    let entry = shortfall.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    shortfall
        .send_controller(vec![
            shortfall.prepare_position_instruction(),
            shortfall.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .unwrap();
    let exit = shortfall.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    let mut vault_account = shortfall.svm.get_account(&shortfall.vault).unwrap();
    let mut vault = TokenAccount::unpack(&vault_account.data).unwrap();
    vault.amount -= 1;
    TokenAccount::pack(vault, &mut vault_account.data).unwrap();
    shortfall
        .svm
        .set_account(shortfall.vault, vault_account)
        .unwrap();
    let before = cash_open_snapshot(&shortfall, &[exit]);
    assert!(shortfall
        .send_controller(vec![
            shortfall.profitable_close_instruction(exit, ProfitSettlementShape::NoClaim,)
        ])
        .is_err());
    assert_eq!(cash_open_snapshot(&shortfall, &[exit]), before);
}

#[test]
fn primary_only_exact_exhaustion_bankruptcy_moves_no_tokens_and_rejects_replay() {
    let mut fixture = Fixture::new(201);
    bootstrap_cash_open(&mut fixture, false);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("open primary-only controller cash position");

    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 99_000_000, 100_000, ORACLE_TIME);
    let source_before = fixture.svm.get_account(&fixture.source).unwrap();
    let vault_before = fixture.svm.get_account(&fixture.vault).unwrap();
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();
    let instruction = fixture.bankruptcy_instruction(exit, None);
    assert_eq!(instruction.accounts.len(), 7);
    let result = fixture
        .send(vec![instruction], false)
        .expect("resolve primary-only exact-exhaustion bankruptcy");
    eprintln!(
        "martingaler_phase2b5_primary_only_exact_bankruptcy_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));
    assert_eq!(
        fixture.svm.get_account(&fixture.source).unwrap(),
        source_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.vault).unwrap(),
        vault_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let market =
        decode_account::<MarketRiskState>(&fixture.svm.get_account(&fixture.market).unwrap().data)
            .unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let position =
        decode_account::<PositionState>(&fixture.svm.get_account(&fixture.position).unwrap().data)
            .unwrap();
    let vault_amount = TokenAccount::unpack(&vault_before.data).unwrap().amount;
    assert_eq!(pool.idle_cash_collateral.get(), 240_000_000);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.free_cash.get(), u128::from(OPEN_COLLATERAL));
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.residual_bad_debt.get(), 0);
    assert_eq!(pool.reserved_open_profit.get(), 0);
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault_amount)
    );
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    assert_eq!(controller.available_cash_collateral.get(), 240_000_000);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(position.reserved_profit.get(), 0);
    assert_eq!(position.entry_price_e6.get(), 100_000_000);

    fixture.svm.expire_blockhash();
    let before_replay = cash_open_snapshot(&fixture, &[exit]);
    assert!(fixture
        .send(vec![fixture.bankruptcy_instruction(exit, None)], false)
        .is_err());
    assert_eq!(cash_open_snapshot(&fixture, &[exit]), before_replay);
}

#[test]
fn permissionless_dual_oracle_bankruptcy_requires_both_and_moves_no_tokens() {
    let mut fixture = Fixture::new(181);
    bootstrap_cash_open(&mut fixture, true);
    let entry_primary =
        fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let entry_secondary =
        fixture.install_switchboard_price(&SECONDARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(
                entry_primary,
                Some(entry_secondary),
                OPEN_COLLATERAL,
                OPEN_NOTIONAL,
                100,
            ),
        ])
        .expect("open dual-oracle position");

    let exit_primary =
        fixture.install_pyth_price(&PRIMARY_FEED_ID, 98_000_000, 100_000, ORACLE_TIME);
    let non_exhausted_secondary =
        fixture.install_switchboard_price(&SECONDARY_FEED_ID, 100_500_000, 100_000, ORACLE_TIME);
    let before_failure = cash_open_snapshot(&fixture, &[exit_primary, non_exhausted_secondary]);
    assert!(fixture
        .send(
            vec![fixture.bankruptcy_instruction(exit_primary, Some(non_exhausted_secondary))],
            false,
        )
        .is_err());
    assert_eq!(
        cash_open_snapshot(&fixture, &[exit_primary, non_exhausted_secondary]),
        before_failure
    );
    fixture.svm.expire_blockhash();

    for account_key in [fixture.pool, fixture.market] {
        let mut account = fixture.svm.get_account(&account_key).unwrap();
        if account_key == fixture.pool {
            let mut state = decode_account::<PoolState>(&account.data).unwrap();
            state.status = PoolStatus::CloseOnly as u8;
            overwrite_account(&mut account.data, &state).unwrap();
        } else {
            let mut state = decode_account::<MarketRiskState>(&account.data).unwrap();
            state.status = PoolStatus::CloseOnly as u8;
            overwrite_account(&mut account.data, &state).unwrap();
        }
        fixture.svm.set_account(account_key, account).unwrap();
    }

    let exhausted_secondary =
        fixture.install_switchboard_price(&SECONDARY_FEED_ID, 98_980_000, 100_000, ORACLE_TIME);
    let source_before = fixture.svm.get_account(&fixture.source).unwrap();
    let vault_before = fixture.svm.get_account(&fixture.vault).unwrap();
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();
    let result = fixture
        .send(
            vec![fixture.bankruptcy_instruction(exit_primary, Some(exhausted_secondary))],
            false,
        )
        .expect("resolve permissionless full bankruptcy");
    eprintln!(
        "martingaler_phase2b5_bankruptcy_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));
    assert_eq!(
        fixture.svm.get_account(&fixture.source).unwrap(),
        source_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.vault).unwrap(),
        vault_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let market =
        decode_account::<MarketRiskState>(&fixture.svm.get_account(&fixture.market).unwrap().data)
            .unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let position =
        decode_account::<PositionState>(&fixture.svm.get_account(&fixture.position).unwrap().data)
            .unwrap();
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.free_cash.get(), u128::from(OPEN_COLLATERAL));
    assert_eq!(pool.residual_bad_debt.get(), u128::from(OPEN_COLLATERAL));
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(position.reserved_profit.get(), 0);

    fixture.svm.expire_blockhash();
    let before_replay = cash_open_snapshot(&fixture, &[exit_primary, exhausted_secondary]);
    assert!(fixture
        .send(
            vec![fixture.bankruptcy_instruction(exit_primary, Some(exhausted_secondary))],
            false,
        )
        .is_err());
    assert_eq!(
        cash_open_snapshot(&fixture, &[exit_primary, exhausted_secondary]),
        before_replay
    );
}

#[test]
fn profitable_and_hostile_close_boundaries_roll_back_every_program_account() {
    for case in 0u8..5 {
        let mut fixture = Fixture::new(182 + case);
        bootstrap_cash_open(&mut fixture, false);
        let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
        fixture
            .send_controller(vec![
                fixture.prepare_position_instruction(),
                fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
            ])
            .unwrap();
        let exit_price = if case == 0 { 101_000_000 } else { 99_500_000 };
        let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, exit_price, 100_000, ORACLE_TIME);
        let mut instruction = fixture.close_cash_instruction(exit);
        match case {
            0 => {}
            1 => instruction.accounts[0].is_signer = false,
            2 => instruction.accounts[11].is_writable = true,
            3 => instruction.accounts[8].pubkey = fixture.vault,
            4 => {
                let mut account = fixture.svm.get_account(&fixture.source).unwrap();
                let mut token = TokenAccount::unpack(&account.data).unwrap();
                token.owner = fixture.admin.pubkey();
                TokenAccount::pack(token, &mut account.data).unwrap();
                fixture.svm.set_account(fixture.source, account).unwrap();
            }
            _ => unreachable!(),
        }
        let before = cash_open_snapshot(&fixture, &[exit]);
        let result = if case == 1 {
            fixture.send(vec![instruction], false)
        } else {
            fixture.send_controller(vec![instruction])
        };
        assert!(result.is_err());
        assert_eq!(cash_open_snapshot(&fixture, &[exit]), before);
    }
}

#[test]
fn fifo_service_partially_pays_exact_available_cash_and_preserves_head() {
    const SERVICE_CASH: u64 = 4_000_000;
    let mut fixture = queued_claim_fixture(111, SERVICE_CASH, PoolStatus::Active);
    let vault_before = TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data)
        .unwrap()
        .amount;
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();
    let result = fixture
        .send(vec![fixture.service_fifo_instruction()], false)
        .expect("permissionlessly service current FIFO head");
    eprintln!(
        "martingaler_phase2b7_partial_service_compute_units={}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let claim =
        decode_account::<ClaimState>(&fixture.svm.get_account(&fixture.claim()).unwrap().data)
            .unwrap();
    let page = decode_account::<QueuePageState>(
        &fixture.svm.get_account(&fixture.page_zero).unwrap().data,
    )
    .unwrap();
    let vault =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data).unwrap();
    let destination = TokenAccount::unpack(
        &fixture
            .svm
            .get_account(&fixture.controller_ata())
            .unwrap()
            .data,
    )
    .unwrap();
    assert_eq!(vault_before - vault.amount, SERVICE_CASH);
    assert_eq!(destination.amount, SERVICE_CASH);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(
        pool.total_queued.get(),
        u128::from(PROFIT_CLAIM_FACE - SERVICE_CASH)
    );
    assert_eq!(pool.head_claim_sequence.get(), 1);
    assert_eq!(pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED, 0);
    assert_eq!(controller.queued_claim_face.get(), pool.total_queued.get());
    assert_eq!(controller.active_claims.get(), 1);
    assert_eq!(claim.status, ClaimStatus::PartiallyPaid as u8);
    assert_eq!(claim.remaining_face.get(), pool.total_queued.get());
    assert_eq!(page.entries[0].remaining_face, claim.remaining_face);
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault.amount)
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );

    fixture.svm.expire_blockhash();
    let before_pop = claim_service_snapshot(&fixture);
    assert!(fixture
        .send(
            vec![fixture.pop_fifo_instruction(PopFifoShape::CurrentPageOrEmpty)],
            false,
        )
        .is_err());
    assert_eq!(claim_service_snapshot(&fixture), before_pop);
    fixture.svm.expire_blockhash();
    assert!(fixture
        .send(vec![fixture.service_fifo_instruction()], false)
        .is_err());
    assert_eq!(claim_service_snapshot(&fixture), before_pop);
}

#[test]
fn full_service_and_final_pop_bundle_is_permissionless_bounded_and_releases_excess() {
    const EXCESS: u64 = 1_000_000;
    let mut fixture = queued_claim_fixture(112, PROFIT_CLAIM_FACE + EXCESS, PoolStatus::ClaimsOnly);
    let vault_before = TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data)
        .unwrap()
        .amount;
    let instructions = vec![
        ComputeBudgetInstruction::request_heap_frame(128 * 1024),
        ComputeBudgetInstruction::set_compute_unit_limit(TX_CU_LIMIT),
        fixture.service_fifo_instruction(),
        fixture.pop_fifo_instruction(PopFifoShape::CurrentPageOrEmpty),
    ];
    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&fixture.payer.pubkey()),
        &[&fixture.payer],
        fixture.svm.latest_blockhash(),
    );
    let wire_len = bincode::serialize(&tx).unwrap().len();
    assert!(wire_len <= 1_232, "service+pop wire length {wire_len}");
    let result = fixture
        .svm
        .send_transaction(tx)
        .expect("permissionlessly service and pop final FIFO head");
    eprintln!(
        "martingaler_phase2b7_service_pop_compute_units={};wire_bytes={wire_len}",
        result.compute_units_consumed
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let claim =
        decode_account::<ClaimState>(&fixture.svm.get_account(&fixture.claim()).unwrap().data)
            .unwrap();
    let page = decode_account::<QueuePageState>(
        &fixture.svm.get_account(&fixture.page_zero).unwrap().data,
    )
    .unwrap();
    let vault =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data).unwrap();
    let destination = TokenAccount::unpack(
        &fixture
            .svm
            .get_account(&fixture.controller_ata())
            .unwrap()
            .data,
    )
    .unwrap();
    assert_eq!(vault_before - vault.amount, PROFIT_CLAIM_FACE);
    assert_eq!(destination.amount, PROFIT_CLAIM_FACE);
    assert_eq!(pool.total_queued.get(), 0);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.free_cash.get(), u128::from(EXCESS));
    assert_eq!(pool.head_claim_sequence.get(), NO_CLAIM_SEQUENCE);
    assert_eq!(pool.tail_claim_sequence.get(), NO_CLAIM_SEQUENCE);
    assert_eq!(pool.queue_flags, 0);
    assert_eq!(controller.queued_claim_face.get(), 0);
    assert_eq!(controller.active_claims.get(), 0);
    assert_eq!(claim.status, ClaimStatus::Settled as u8);
    assert_eq!(claim.remaining_face.get(), 0);
    assert!(page.entries[0].is_zero());
    assert_eq!(page.head_offset.get(), 1);
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault.amount)
    );

    fixture.svm.expire_blockhash();
    let before_replay = claim_service_snapshot(&fixture);
    assert!(fixture
        .send(vec![fixture.service_fifo_instruction()], false)
        .is_err());
    assert_eq!(claim_service_snapshot(&fixture), before_replay);
    fixture.svm.expire_blockhash();
    assert!(fixture
        .send(
            vec![fixture.pop_fifo_instruction(PopFifoShape::CurrentPageOrEmpty)],
            false,
        )
        .is_err());
    assert_eq!(claim_service_snapshot(&fixture), before_replay);
}

#[test]
fn fifo_service_enforces_canonical_ata_roles_relations_and_modes_atomically() {
    for case in 0u8..13 {
        let mut fixture = queued_claim_fixture(
            120u8.wrapping_add(case),
            1_000_000,
            if case == 12 {
                PoolStatus::Terminal
            } else {
                PoolStatus::CloseOnly
            },
        );
        let mut instruction = fixture.service_fifo_instruction();
        match case {
            0 => {
                instruction.accounts.pop();
            }
            1 => instruction.accounts[6] = AccountMeta::new(fixture.source, false),
            2 => instruction.accounts[2] = AccountMeta::new(fixture.page_zero, false),
            3 => instruction.accounts[0] = AccountMeta::new_readonly(fixture.pool, false),
            4 => {
                instruction.accounts[1] = AccountMeta::new_readonly(fixture.controller_risk, false)
            }
            5 => instruction.accounts[2] = AccountMeta::new_readonly(fixture.claim(), false),
            6 => instruction.accounts[3] = AccountMeta::new_readonly(fixture.page_zero, false),
            7 => instruction.accounts[5] = AccountMeta::new_readonly(fixture.vault, false),
            8 => {
                instruction.accounts[6] = AccountMeta::new_readonly(fixture.controller_ata(), false)
            }
            9 => instruction.accounts[7] = AccountMeta::new(fixture.quote_mint, false),
            10 => instruction.accounts[8] = AccountMeta::new_readonly(system_program::ID, false),
            11 => {
                let mut claim_account = fixture.svm.get_account(&fixture.claim()).unwrap();
                claim_account.data[12] ^= 1;
                fixture
                    .svm
                    .set_account(fixture.claim(), claim_account)
                    .unwrap();
            }
            12 => {}
            _ => unreachable!(),
        }
        let before = claim_service_snapshot(&fixture);
        assert!(
            fixture.send(vec![instruction], false).is_err(),
            "hostile service case {case}"
        );
        assert_eq!(claim_service_snapshot(&fixture), before, "case {case}");
    }
}

#[test]
fn fifo_service_rejects_corrupt_claim_controller_page_token_and_custody_state_atomically() {
    for case in 0u8..16 {
        let mut fixture =
            queued_claim_fixture(150u8.wrapping_add(case), 1_000_000, PoolStatus::CloseOnly);
        let claim_key = fixture.claim();
        let page_zero = fixture.page_zero;
        match case {
            0..=2 => {
                let mut account = fixture.svm.get_account(&fixture.claim()).unwrap();
                let mut state = decode_account::<ClaimState>(&account.data).unwrap();
                match case {
                    0 => state.sequence = U128Le::new(2),
                    1 => state.queue_slot = U16Le::new(1),
                    2 => state.queue_page_index = percolator_prog::martingaler_state::U64Le::new(1),
                    _ => unreachable!(),
                }
                overwrite_account(&mut account.data, &state).unwrap();
                fixture.svm.set_account(fixture.claim(), account).unwrap();
            }
            3 | 4 => {
                let mut account = fixture.svm.get_account(&fixture.controller_risk).unwrap();
                let mut state = decode_account::<ControllerRiskState>(&account.data).unwrap();
                if case == 3 {
                    state.queued_claim_face = U128Le::new(0);
                } else {
                    state.active_claims = percolator_prog::martingaler_state::U32Le::new(0);
                }
                overwrite_account(&mut account.data, &state).unwrap();
                fixture
                    .svm
                    .set_account(fixture.controller_risk, account)
                    .unwrap();
            }
            5 | 6 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                let mut state = decode_account::<QueuePageState>(&account.data).unwrap();
                if case == 5 {
                    state.entries[0].claim = Key32::new([0xDD; 32]);
                } else {
                    state.entries[0].remaining_face = U128Le::new(PROFIT_CLAIM_FACE as u128 - 1);
                }
                overwrite_account(&mut account.data, &state).unwrap();
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            7 => {
                let mut account = fixture.svm.get_account(&fixture.pool).unwrap();
                let mut state = decode_account::<PoolState>(&account.data).unwrap();
                state.head_claim_sequence = U128Le::new(2);
                state.tail_claim_sequence = U128Le::new(2);
                state.next_claim_sequence = U128Le::new(3);
                overwrite_account(&mut account.data, &state).unwrap();
                fixture.svm.set_account(fixture.pool, account).unwrap();
            }
            8 => {
                let mut account = fixture.svm.get_account(&fixture.vault).unwrap();
                let mut token = TokenAccount::unpack(&account.data).unwrap();
                token.amount -= 1;
                TokenAccount::pack(token, &mut account.data).unwrap();
                fixture.svm.set_account(fixture.vault, account).unwrap();
            }
            9 | 10 => {
                let mut account = fixture.svm.get_account(&fixture.controller_ata()).unwrap();
                let mut token = TokenAccount::unpack(&account.data).unwrap();
                if case == 9 {
                    token.state = AccountState::Frozen;
                } else {
                    token.owner = fixture.admin.pubkey();
                }
                TokenAccount::pack(token, &mut account.data).unwrap();
                fixture
                    .svm
                    .set_account(fixture.controller_ata(), account)
                    .unwrap();
            }
            11 => replace_account(
                &mut fixture,
                claim_key,
                system_program::ID,
                account_len::<ClaimState>(),
                true,
            ),
            12 => replace_account(
                &mut fixture,
                page_zero,
                id(),
                account_len::<QueuePageState>() - 1,
                true,
            ),
            13 => {
                let mut account = fixture.svm.get_account(&fixture.vault).unwrap();
                let mut token = TokenAccount::unpack(&account.data).unwrap();
                token.delegate = COption::Some(fixture.admin.pubkey());
                token.delegated_amount = 1;
                TokenAccount::pack(token, &mut account.data).unwrap();
                fixture.svm.set_account(fixture.vault, account).unwrap();
            }
            14 => {
                let mut account = fixture.svm.get_account(&fixture.controller_risk).unwrap();
                let mut state = decode_account::<ControllerRiskState>(&account.data).unwrap();
                state.queued_claim_face = U128Le::new(1_000_000);
                overwrite_account(&mut account.data, &state).unwrap();
                fixture
                    .svm
                    .set_account(fixture.controller_risk, account)
                    .unwrap();
            }
            15 => {
                let mut account = fixture.svm.get_account(&fixture.pool).unwrap();
                let mut state = decode_account::<PoolState>(&account.data).unwrap();
                state.total_queued = U128Le::new(1_000_000);
                overwrite_account(&mut account.data, &state).unwrap();
                fixture.svm.set_account(fixture.pool, account).unwrap();
            }
            _ => unreachable!(),
        }
        let before = claim_service_snapshot(&fixture);
        assert!(
            fixture
                .send(vec![fixture.service_fifo_instruction()], false)
                .is_err(),
            "corrupt service case {case}"
        );
        assert_eq!(claim_service_snapshot(&fixture), before, "case {case}");
    }
}

#[test]
fn fifo_pop_advances_same_page_and_rejects_hostile_shapes_state_and_roles_atomically() {
    let mut same_page = fully_serviced_claim_fixture(180);
    let mut page_account = same_page.svm.get_account(&same_page.page_zero).unwrap();
    let mut page = decode_account::<QueuePageState>(&page_account.data).unwrap();
    page.entries[1] = QueueEntry {
        claim: Key32::new([0xEE; 32]),
        sequence: U128Le::new(2),
        remaining_face: U128Le::new(1),
    };
    page.len = U16Le::new(2);
    page.next_append_sequence = U128Le::new(3);
    overwrite_account(&mut page_account.data, &page).unwrap();
    same_page
        .svm
        .set_account(same_page.page_zero, page_account)
        .unwrap();
    let mut pool_account = same_page.svm.get_account(&same_page.pool).unwrap();
    let mut pool = decode_account::<PoolState>(&pool_account.data).unwrap();
    pool.total_queued = U128Le::new(1);
    pool.next_claim_sequence = U128Le::new(3);
    pool.tail_claim_sequence = U128Le::new(2);
    overwrite_account(&mut pool_account.data, &pool).unwrap();
    same_page
        .svm
        .set_account(same_page.pool, pool_account)
        .unwrap();
    same_page
        .send(
            vec![same_page.pop_fifo_instruction(PopFifoShape::CurrentPageOrEmpty)],
            false,
        )
        .expect("advance settled head within current page");
    let pool =
        decode_account::<PoolState>(&same_page.svm.get_account(&same_page.pool).unwrap().data)
            .unwrap();
    assert_eq!(pool.head_claim_sequence.get(), 2);
    assert_eq!(pool.total_queued.get(), 1);
    assert_eq!(pool.queue_flags, 0);

    for case in 0u8..13 {
        let mut fixture = fully_serviced_claim_fixture(190u8.wrapping_add(case));
        let claim_key = fixture.claim();
        let mut instruction = fixture.pop_fifo_instruction(PopFifoShape::CurrentPageOrEmpty);
        match case {
            0 => {
                instruction.accounts.pop();
            }
            1 => instruction = fixture.pop_fifo_instruction(PopFifoShape::NextPage),
            2 => instruction.accounts[0] = AccountMeta::new_readonly(fixture.pool, false),
            3 => instruction.accounts[1] = AccountMeta::new_readonly(fixture.claim(), false),
            4 => instruction.accounts[2] = AccountMeta::new_readonly(fixture.page_zero, false),
            5 => instruction.accounts[1] = AccountMeta::new(fixture.page_zero, false),
            6 => {
                let mut account = fixture.svm.get_account(&fixture.claim()).unwrap();
                account.data[12] ^= 1;
                fixture.svm.set_account(fixture.claim(), account).unwrap();
            }
            7 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                account.data[12] ^= 1;
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            8 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                let mut state = decode_account::<QueuePageState>(&account.data).unwrap();
                state.entries[0].claim = Key32::new([0xEF; 32]);
                overwrite_account(&mut account.data, &state).unwrap();
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            9 => {
                let mut account = fixture.svm.get_account(&fixture.pool).unwrap();
                let mut state = decode_account::<PoolState>(&account.data).unwrap();
                state.status = PoolStatus::Terminal as u8;
                overwrite_account(&mut account.data, &state).unwrap();
                fixture.svm.set_account(fixture.pool, account).unwrap();
            }
            10 => instruction
                .accounts
                .push(AccountMeta::new_readonly(system_program::ID, false)),
            11 => replace_account(
                &mut fixture,
                claim_key,
                system_program::ID,
                account_len::<ClaimState>(),
                true,
            ),
            12 => {
                let mut account = fixture.svm.get_account(&fixture.page_zero).unwrap();
                account.lamports = 1;
                fixture.svm.set_account(fixture.page_zero, account).unwrap();
            }
            _ => unreachable!(),
        }
        let before = claim_service_snapshot(&fixture);
        assert!(
            fixture.send(vec![instruction], false).is_err(),
            "hostile pop case {case}"
        );
        assert_eq!(claim_service_snapshot(&fixture), before, "case {case}");
    }
}

#[test]
fn fifo_pop_crosses_exactly_one_canonical_page_and_preserves_next_page() {
    let mut fixture = cross_page_queued_claim_fixture(140);
    let next_before = fixture.svm.get_account(&fixture.queue_page(1)).unwrap();
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();
    let vault_before = TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data)
        .unwrap()
        .amount;
    let instructions = vec![
        ComputeBudgetInstruction::request_heap_frame(128 * 1024),
        ComputeBudgetInstruction::set_compute_unit_limit(TX_CU_LIMIT),
        fixture.service_fifo_instruction(),
        fixture.pop_fifo_instruction(PopFifoShape::NextPage),
    ];
    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&fixture.payer.pubkey()),
        &[&fixture.payer],
        fixture.svm.latest_blockhash(),
    );
    let wire_len = bincode::serialize(&tx).unwrap().len();
    assert!(
        wire_len <= 1_232,
        "cross-page bundle wire length {wire_len}"
    );
    let result = fixture
        .svm
        .send_transaction(tx)
        .expect("fully service and advance FIFO head across one canonical page");
    eprintln!(
        "martingaler_phase2b7_cross_page_bundle_compute_units={};wire_bytes={wire_len}",
        result.compute_units_consumed,
    );
    assert!(result.compute_units_consumed <= u64::from(TX_CU_LIMIT));
    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let claim =
        decode_account::<ClaimState>(&fixture.svm.get_account(&fixture.claim()).unwrap().data)
            .unwrap();
    let first = decode_account::<QueuePageState>(
        &fixture.svm.get_account(&fixture.page_zero).unwrap().data,
    )
    .unwrap();
    let vault =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data).unwrap();
    let destination = TokenAccount::unpack(
        &fixture
            .svm
            .get_account(&fixture.controller_ata())
            .unwrap()
            .data,
    )
    .unwrap();
    assert_eq!(vault_before - vault.amount, PROFIT_CLAIM_FACE);
    assert_eq!(destination.amount, PROFIT_CLAIM_FACE);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.head_page_index.get(), 1);
    assert_eq!(pool.head_claim_sequence.get(), 33);
    assert_eq!(pool.tail_claim_sequence.get(), 33);
    assert_eq!(pool.total_queued.get(), 1);
    assert_eq!(pool.queue_flags, 0);
    assert_eq!(controller.queued_claim_face.get(), 0);
    assert_eq!(controller.active_claims.get(), 0);
    assert_eq!(claim.status, ClaimStatus::Settled as u8);
    assert_eq!(claim.remaining_face.get(), 0);
    assert!(first.entries[31].is_zero());
    assert_eq!(first.head_offset.get(), 32);
    assert_eq!(
        fixture.svm.get_account(&fixture.queue_page(1)).unwrap(),
        next_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault.amount)
    );
}

#[test]
fn cross_page_pop_rejects_hostile_next_page_boundary_atomically() {
    for case in 0u8..12 {
        let mut fixture = fully_serviced_cross_page_claim_fixture(210 + case);
        let next_key = fixture.queue_page(1);
        let mut instruction = fixture.pop_fifo_instruction(PopFifoShape::NextPage);
        match case {
            0 => {
                instruction.accounts.pop();
            }
            1 => instruction
                .accounts
                .push(AccountMeta::new_readonly(system_program::ID, false)),
            2 => instruction.accounts[3] = AccountMeta::new(next_key, false),
            3 => instruction.accounts[3] = AccountMeta::new_readonly(fixture.claim(), false),
            4 => {
                let wrong_key = fixture.queue_page(2);
                let account = fixture.svm.get_account(&next_key).unwrap();
                fixture.svm.set_account(wrong_key, account).unwrap();
                instruction.accounts[3] = AccountMeta::new_readonly(wrong_key, false);
            }
            5 => {
                let mut account = fixture.svm.get_account(&next_key).unwrap();
                account.owner = system_program::ID;
                fixture.svm.set_account(next_key, account).unwrap();
            }
            6 => {
                let mut account = fixture.svm.get_account(&next_key).unwrap();
                account.data.pop();
                fixture.svm.set_account(next_key, account).unwrap();
            }
            7 => {
                let mut account = fixture.svm.get_account(&next_key).unwrap();
                account.lamports = 1;
                fixture.svm.set_account(next_key, account).unwrap();
            }
            8 => {
                let mut account = fixture.svm.get_account(&next_key).unwrap();
                account.data[12] ^= 1;
                fixture.svm.set_account(next_key, account).unwrap();
            }
            9 => {
                let mut account = fixture.svm.get_account(&next_key).unwrap();
                account.data[0] ^= 1;
                fixture.svm.set_account(next_key, account).unwrap();
            }
            10 | 11 => {
                let mut account = fixture.svm.get_account(&next_key).unwrap();
                if case == 10 {
                    account.data[ACCOUNT_HEADER_LEN + 40..ACCOUNT_HEADER_LEN + 48]
                        .copy_from_slice(&9u64.to_le_bytes());
                } else {
                    let mut page = decode_account::<QueuePageState>(&account.data).unwrap();
                    page.first_sequence = U128Le::new(34);
                    page.next_append_sequence = U128Le::new(35);
                    page.entries[0].sequence = U128Le::new(34);
                    overwrite_account(&mut account.data, &page).unwrap();
                }
                fixture.svm.set_account(next_key, account).unwrap();
            }
            _ => unreachable!(),
        }
        let before = claim_service_snapshot(&fixture);
        assert!(
            fixture.send(vec![instruction], false).is_err(),
            "hostile cross-page case {case}"
        );
        assert_eq!(claim_service_snapshot(&fixture), before, "case {case}");
    }
}

#[test]
fn hostile_bankruptcy_oracle_and_privilege_shapes_are_atomic() {
    for case in 0u8..7 {
        let mut fixture = Fixture::new(190 + case);
        bootstrap_cash_open(&mut fixture, true);
        let entry_primary =
            fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
        let entry_secondary = fixture.install_switchboard_price(
            &SECONDARY_FEED_ID,
            101_000_000,
            100_000,
            ORACLE_TIME,
        );
        fixture
            .send_controller(vec![
                fixture.prepare_position_instruction(),
                fixture.open_cash_instruction(
                    entry_primary,
                    Some(entry_secondary),
                    OPEN_COLLATERAL,
                    OPEN_NOTIONAL,
                    100,
                ),
            ])
            .unwrap();
        let (primary_price, primary_time, primary_feed) = match case {
            5 => (98_000_000, ORACLE_TIME - 31, PRIMARY_FEED_ID),
            6 => (98_000_000, ORACLE_TIME, [0x44; 32]),
            _ => (98_000_000, ORACLE_TIME, PRIMARY_FEED_ID),
        };
        let primary =
            fixture.install_pyth_price(&primary_feed, primary_price, 100_000, primary_time);
        let secondary = if case == 3 {
            fixture.install_pyth_price(&SECONDARY_FEED_ID, 98_980_000, 100_000, ORACLE_TIME)
        } else {
            fixture.install_switchboard_price(
                &SECONDARY_FEED_ID,
                if case == 4 { 110_000_000 } else { 98_980_000 },
                100_000,
                ORACLE_TIME,
            )
        };
        let mut instruction = fixture.bankruptcy_instruction(primary, Some(secondary));
        match case {
            0 => {
                instruction.accounts.pop();
            }
            1 => instruction.accounts[5].is_writable = true,
            2 => instruction.accounts[6].is_writable = true,
            3..=6 => {}
            _ => unreachable!(),
        }
        let before = cash_open_snapshot(&fixture, &[primary, secondary]);
        assert!(fixture.send(vec![instruction], false).is_err());
        assert_eq!(cash_open_snapshot(&fixture, &[primary, secondary]), before);
    }
}

#[test]
fn claim_funded_flat_close_preserves_contingent_collateral_as_a_child_fifo_claim() {
    let mut fixture = opened_claim_position_fixture(230);
    let source_claim_key = fixture.claim();
    let child_claim_key = fixture.claim_for(CLAIM_POSITION_ID);
    let vault_before = fixture.svm.get_account(&fixture.vault).unwrap();
    let destination_before = fixture.svm.get_account(&fixture.source).unwrap();

    let open_position = decode_account::<PositionState>(
        &fixture
            .svm
            .get_account(&fixture.position_for(CLAIM_POSITION_ID))
            .unwrap()
            .data,
    )
    .unwrap();
    let source_after_open =
        decode_account::<ClaimState>(&fixture.svm.get_account(&source_claim_key).unwrap().data)
            .unwrap();
    let pool_after_open =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    assert_eq!(open_position.funding_kind, FundingKind::Claim as u8);
    assert_eq!(
        open_position.source_claim_sequence,
        source_after_open.sequence
    );
    assert_eq!(source_after_open.remaining_face.get(), 0);
    assert_eq!(
        source_after_open.collateral_in_use.get(),
        u128::from(PROFIT_CLAIM_FACE)
    );
    assert_eq!(pool_after_open.total_queued.get(), 0);
    assert_eq!(
        pool_after_open.open_claim_collateral.get(),
        u128::from(PROFIT_CLAIM_FACE)
    );
    assert_ne!(pool_after_open.queue_flags & QUEUE_FLAG_HEAD_SETTLED, 0);

    let flat = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_claim_instruction_for(CLAIM_POSITION_ID),
            fixture.close_claim_instruction(
                CLAIM_POSITION_ID,
                source_claim_key,
                flat,
                ProfitSettlementShape::CurrentTail,
                false,
            ),
        ])
        .expect("close flat claim-funded position into child FIFO claim");

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let source =
        decode_account::<ClaimState>(&fixture.svm.get_account(&source_claim_key).unwrap().data)
            .unwrap();
    let child =
        decode_account::<ClaimState>(&fixture.svm.get_account(&child_claim_key).unwrap().data)
            .unwrap();
    let position = decode_account::<PositionState>(
        &fixture
            .svm
            .get_account(&fixture.position_for(CLAIM_POSITION_ID))
            .unwrap()
            .data,
    )
    .unwrap();
    assert_eq!(source.status, ClaimStatus::Settled as u8);
    assert_eq!(source.collateral_in_use.get(), 0);
    assert_eq!(child.status, ClaimStatus::Queued as u8);
    assert_eq!(child.funding_kind, FundingKind::Claim as u8);
    assert_eq!(child.source_claim_sequence, source.sequence);
    assert_eq!(child.remaining_face.get(), u128::from(PROFIT_CLAIM_FACE));
    assert!(child.sequence.get() > source.sequence.get());
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(pool.open_claim_collateral.get(), 0);
    assert_eq!(pool.total_queued.get(), u128::from(PROFIT_CLAIM_FACE));
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(controller.active_claims.get(), 1);
    assert_eq!(
        fixture.svm.get_account(&fixture.vault).unwrap(),
        vault_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.source).unwrap(),
        destination_before
    );
}

#[test]
fn claim_funded_dual_oracle_flat_close_requires_both_feeds_and_preserves_fifo_value() {
    let mut fixture = opened_claim_position_dual_fixture(233);
    let source_claim_key = fixture.claim();
    let child_claim_key = fixture.claim_for(CLAIM_POSITION_ID);
    let primary = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let secondary =
        fixture.install_switchboard_price(&SECONDARY_FEED_ID, 100_200_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_claim_instruction_for(CLAIM_POSITION_ID),
            fixture.close_claim_instruction_with_oracles(
                CLAIM_POSITION_ID,
                source_claim_key,
                primary,
                Some(secondary),
                ProfitSettlementShape::CurrentTail,
                false,
            ),
        ])
        .expect("close dual-oracle claim-funded position");

    let source =
        decode_account::<ClaimState>(&fixture.svm.get_account(&source_claim_key).unwrap().data)
            .unwrap();
    let child =
        decode_account::<ClaimState>(&fixture.svm.get_account(&child_claim_key).unwrap().data)
            .unwrap();
    let position = decode_account::<PositionState>(
        &fixture
            .svm
            .get_account(&fixture.position_for(CLAIM_POSITION_ID))
            .unwrap()
            .data,
    )
    .unwrap();
    assert_eq!(source.status, ClaimStatus::Settled as u8);
    assert_eq!(child.funding_kind, FundingKind::Claim as u8);
    assert_eq!(child.remaining_face.get(), u128::from(PROFIT_CLAIM_FACE));
    assert_eq!(position.status, PositionStatus::Closed as u8);
}

#[test]
fn claim_funded_profitable_close_queues_adjusted_collateral_plus_profit_without_cash_transfer() {
    let mut fixture = opened_claim_position_fixture(231);
    let source_claim_key = fixture.claim();
    let child_claim_key = fixture.claim_for(CLAIM_POSITION_ID);
    let vault_before = fixture.svm.get_account(&fixture.vault).unwrap();
    let destination_before = fixture.svm.get_account(&fixture.source).unwrap();
    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_claim_instruction_for(CLAIM_POSITION_ID),
            fixture.close_claim_instruction(
                CLAIM_POSITION_ID,
                source_claim_key,
                exit,
                ProfitSettlementShape::CurrentTail,
                true,
            ),
        ])
        .expect("close profitable claim-funded position into child FIFO claim");

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let source =
        decode_account::<ClaimState>(&fixture.svm.get_account(&source_claim_key).unwrap().data)
            .unwrap();
    let child =
        decode_account::<ClaimState>(&fixture.svm.get_account(&child_claim_key).unwrap().data)
            .unwrap();
    assert_eq!(source.status, ClaimStatus::Settled as u8);
    assert_eq!(child.funding_kind, FundingKind::Claim as u8);
    assert_eq!(child.source_claim_sequence, source.sequence);
    assert!(child.remaining_face.get() > u128::from(PROFIT_CLAIM_FACE));
    assert_eq!(pool.total_queued.get(), child.remaining_face.get());
    assert_eq!(pool.open_claim_collateral.get(), 0);
    assert!(pool.fee_accrual.get() > 0);
    assert_eq!(
        fixture.svm.get_account(&fixture.vault).unwrap(),
        vault_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.source).unwrap(),
        destination_before
    );
}

#[test]
fn claim_funded_close_pays_immediately_only_after_priority_clears_and_free_cash_exists() {
    let mut fixture = opened_claim_position_fixture(234);
    let source_claim_key = fixture.claim();
    fixture
        .send(
            vec![fixture.pop_fifo_instruction(PopFifoShape::CurrentPageOrEmpty)],
            false,
        )
        .expect("pop fully consumed source claim while it funds a position");

    const FREE_CASH: u128 = 20_000_000;
    let mut pool_account = fixture.svm.get_account(&fixture.pool).unwrap();
    let mut pool_state = decode_account::<PoolState>(&pool_account.data).unwrap();
    pool_state.idle_cash_collateral = U128Le::new(
        pool_state
            .idle_cash_collateral
            .get()
            .checked_sub(FREE_CASH)
            .unwrap(),
    );
    pool_state.free_cash = U128Le::new(FREE_CASH);
    overwrite_account(&mut pool_account.data, &pool_state).unwrap();
    fixture.svm.set_account(fixture.pool, pool_account).unwrap();

    let vault_before = TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data)
        .unwrap()
        .amount;
    let destination_before =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.source).unwrap().data)
            .unwrap()
            .amount;
    let flat = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![fixture.close_claim_instruction(
            CLAIM_POSITION_ID,
            source_claim_key,
            flat,
            ProfitSettlementShape::NoClaim,
            false,
        )])
        .expect("pay claim-funded collateral from available free cash");

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let source =
        decode_account::<ClaimState>(&fixture.svm.get_account(&source_claim_key).unwrap().data)
            .unwrap();
    let vault_after = TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data)
        .unwrap()
        .amount;
    let destination_after =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.source).unwrap().data)
            .unwrap()
            .amount;
    assert_eq!(source.status, ClaimStatus::Settled as u8);
    assert_eq!(pool.total_queued.get(), 0);
    assert_eq!(pool.open_claim_collateral.get(), 0);
    assert_eq!(
        pool.free_cash.get(),
        FREE_CASH - u128::from(PROFIT_CLAIM_FACE)
    );
    assert_eq!(vault_before - vault_after, PROFIT_CLAIM_FACE);
    assert_eq!(destination_after - destination_before, PROFIT_CLAIM_FACE);
    assert!(fixture
        .svm
        .get_account(&fixture.claim_for(CLAIM_POSITION_ID))
        .is_none());
}

#[test]
fn claim_funded_bankruptcy_is_permissionless_extinguishes_claim_and_never_transfers_cash() {
    let mut fixture = opened_claim_position_fixture(232);
    let source_claim_key = fixture.claim();
    let vault_before = fixture.svm.get_account(&fixture.vault).unwrap();
    let destination_before = fixture.svm.get_account(&fixture.source).unwrap();
    let exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 98_000_000, 100_000, ORACLE_TIME);
    fixture
        .send(
            vec![fixture.claim_bankruptcy_instruction(
                CLAIM_POSITION_ID,
                source_claim_key,
                exit,
                None,
            )],
            false,
        )
        .expect("permissionlessly resolve bankrupt claim-funded position");

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let source =
        decode_account::<ClaimState>(&fixture.svm.get_account(&source_claim_key).unwrap().data)
            .unwrap();
    let position = decode_account::<PositionState>(
        &fixture
            .svm
            .get_account(&fixture.position_for(CLAIM_POSITION_ID))
            .unwrap()
            .data,
    )
    .unwrap();
    assert_eq!(source.status, ClaimStatus::Extinguished as u8);
    assert_eq!(source.remaining_face.get(), 0);
    assert_eq!(source.collateral_in_use.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(pool.total_queued.get(), 0);
    assert_eq!(pool.open_claim_collateral.get(), 0);
    assert_eq!(pool.residual_bad_debt.get(), 8_820_000);
    assert_eq!(controller.active_claims.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(
        fixture.svm.get_account(&fixture.vault).unwrap(),
        vault_before
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.source).unwrap(),
        destination_before
    );
}

#[test]
fn claim_close_wrong_settlement_shape_is_atomic() {
    let mut fixture = opened_claim_position_fixture(233);
    let source_claim_key = fixture.claim();
    let flat = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let keys = [
        fixture.pool,
        fixture.market,
        fixture.controller_risk,
        fixture.position_for(CLAIM_POSITION_ID),
        source_claim_key,
        fixture.page_zero,
        fixture.vault,
        fixture.source,
    ];
    let before = account_snapshot(&fixture, &keys);
    let instruction = fixture.close_claim_instruction(
        CLAIM_POSITION_ID,
        source_claim_key,
        flat,
        ProfitSettlementShape::NoClaim,
        false,
    );
    assert!(fixture.send_controller(vec![instruction]).is_err());
    assert_eq!(account_snapshot(&fixture, &keys), before);
}

#[test]
fn devnet_release_rehearsal_runs_cash_claim_fifo_reuse_and_wind_down_on_fresh_sbf() {
    const CLAIM_POSITION_ID: u64 = POSITION_ID + 1;
    const LOSS_POSITION_ID: u64 = POSITION_ID + 2;
    let mut fixture = Fixture::new(237);
    bootstrap_cash_open(&mut fixture, false);
    install_controller_ata(&mut fixture);
    let mint_before = fixture.svm.get_account(&fixture.quote_mint).unwrap();
    let mut max_compute_units = 0u64;

    let cash_entry =
        fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let result = fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(cash_entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .expect("release rehearsal opens the cash-funded position");
    max_compute_units = max_compute_units.max(result.compute_units_consumed);

    let cash_profit =
        fixture.install_pyth_price(&PRIMARY_FEED_ID, 101_000_000, 100_000, ORACLE_TIME);
    let result = fixture
        .send_controller(vec![
            fixture.prepare_claim_instruction(),
            fixture.profitable_close_instruction(cash_profit, ProfitSettlementShape::CurrentTail),
        ])
        .expect("release rehearsal queues profit without seeded liquidity");
    max_compute_units = max_compute_units.max(result.compute_units_consumed);
    let source_claim = fixture.claim();
    let queued =
        decode_account::<ClaimState>(&fixture.svm.get_account(&source_claim).unwrap().data)
            .unwrap();
    assert_eq!(queued.status, ClaimStatus::Queued as u8);
    assert_eq!(queued.remaining_face.get(), u128::from(PROFIT_CLAIM_FACE));

    let claim_entry =
        fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let result = fixture
        .send_controller(vec![
            fixture.prepare_position_instruction_for(CLAIM_POSITION_ID),
            fixture.open_claim_instruction(
                CLAIM_POSITION_ID,
                source_claim,
                fixture.page_zero,
                claim_entry,
                None,
                PROFIT_CLAIM_FACE,
                882_000_000,
                100,
            ),
        ])
        .expect("release rehearsal reuses queued profit as contingent collateral");
    max_compute_units = max_compute_units.max(result.compute_units_consumed);
    fixture
        .send(
            vec![fixture.pop_fifo_instruction_for(
                source_claim,
                fixture.page_zero,
                PopFifoShape::CurrentPageOrEmpty,
            )],
            false,
        )
        .expect("release rehearsal pops the settled source priority");

    let claim_flat =
        fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    let child_claim = fixture.claim_for(CLAIM_POSITION_ID);
    let result = fixture
        .send_controller(vec![
            fixture.prepare_claim_instruction_for(CLAIM_POSITION_ID),
            fixture.close_claim_instruction(
                CLAIM_POSITION_ID,
                source_claim,
                claim_flat,
                ProfitSettlementShape::CurrentTail,
                false,
            ),
        ])
        .expect("release rehearsal preserves contingent value in a child FIFO claim");
    max_compute_units = max_compute_units.max(result.compute_units_consumed);
    let child =
        decode_account::<ClaimState>(&fixture.svm.get_account(&child_claim).unwrap().data).unwrap();
    assert_eq!(child.sequence.get(), 2);
    assert_eq!(child.source_claim_sequence.get(), 1);
    assert_eq!(child.remaining_face.get(), u128::from(PROFIT_CLAIM_FACE));

    let loss_entry =
        fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction_for(LOSS_POSITION_ID),
            fixture.open_cash_instruction_for(
                LOSS_POSITION_ID,
                loss_entry,
                None,
                OPEN_COLLATERAL,
                OPEN_NOTIONAL,
                100,
            ),
        ])
        .expect("release rehearsal opens the loss-funding cash position");
    let loss_exit = fixture.install_pyth_price(&PRIMARY_FEED_ID, 99_100_000, 100_000, ORACLE_TIME);
    let result = fixture
        .send_controller(vec![fixture.close_cash_instruction_for(
            LOSS_POSITION_ID,
            loss_exit,
            None,
        )])
        .expect("release rehearsal routes trader loss into claim service cash");
    max_compute_units = max_compute_units.max(result.compute_units_consumed);
    let funded_pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    assert!(funded_pool.claim_service_cash.get() >= u128::from(PROFIT_CLAIM_FACE));

    let result = fixture
        .send(
            vec![fixture.service_fifo_instruction_for(child_claim, fixture.page_zero)],
            false,
        )
        .expect("release rehearsal permissionlessly services the exact child head");
    max_compute_units = max_compute_units.max(result.compute_units_consumed);
    fixture
        .send(
            vec![fixture.pop_fifo_instruction_for(
                child_claim,
                fixture.page_zero,
                PopFifoShape::CurrentPageOrEmpty,
            )],
            false,
        )
        .expect("release rehearsal permissionlessly pops the paid child head");

    fixture
        .send(
            vec![fixture.set_market_status_instruction(PoolStatus::CloseOnly)],
            true,
        )
        .expect("release rehearsal closes the market to new risk");
    fixture
        .send(
            vec![fixture.set_pool_status_instruction(PoolStatus::CloseOnly)],
            true,
        )
        .expect("release rehearsal closes the pool to new risk");
    fixture
        .send(
            vec![fixture.set_market_status_instruction(PoolStatus::Terminal)],
            true,
        )
        .expect("release rehearsal terminalizes the drained market");
    fixture
        .send(
            vec![fixture.set_pool_status_instruction(PoolStatus::ClaimsOnly)],
            true,
        )
        .expect("release rehearsal enters claims-only mode");
    fixture
        .send(
            vec![fixture.set_pool_status_instruction(PoolStatus::Terminal)],
            true,
        )
        .expect("release rehearsal terminalizes the drained pool");

    let before_withdraw = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let idle = u64::try_from(before_withdraw.available_cash_collateral.get()).unwrap();
    fixture
        .send_controller(vec![fixture.withdraw_idle_cash_instruction(idle)])
        .expect("release rehearsal returns every remaining controller cash atom");

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let market =
        decode_account::<MarketRiskState>(&fixture.svm.get_account(&fixture.market).unwrap().data)
            .unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let vault =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data).unwrap();
    assert_eq!(pool.status, PoolStatus::Terminal as u8);
    assert_eq!(market.status, PoolStatus::Terminal as u8);
    assert_eq!(pool.total_queued.get(), 0);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.open_claim_collateral.get(), 0);
    assert_eq!(pool.reserved_open_profit.get(), 0);
    assert_eq!(controller.available_cash_collateral.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(controller.active_claims.get(), 0);
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault.amount)
    );
    assert_eq!(
        fixture.svm.get_account(&fixture.quote_mint).unwrap(),
        mint_before
    );
    assert!(max_compute_units <= u64::from(TX_CU_LIMIT));
    eprintln!("martingaler_devnet_release_rehearsal_max_compute_units={max_compute_units}");
}

#[test]
fn real_sbf_wind_down_and_idle_cash_withdrawal_complete_without_admin_bankroll_access() {
    let mut fixture = Fixture::new(235);
    bootstrap_cash_open(&mut fixture, false);
    let source_before =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.source).unwrap().data)
            .unwrap()
            .amount;
    let vault_before = TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data)
        .unwrap()
        .amount;

    fixture
        .send(
            vec![fixture.set_market_status_instruction(PoolStatus::CloseOnly)],
            true,
        )
        .expect("stop new market risk");
    fixture
        .send(
            vec![fixture.set_pool_status_instruction(PoolStatus::CloseOnly)],
            true,
        )
        .expect("stop new pool risk");
    fixture
        .send_controller(vec![fixture.withdraw_idle_cash_instruction(100_000_000)])
        .expect("controller withdraws its own idle collateral in close-only mode");
    fixture
        .send(
            vec![fixture.set_market_status_instruction(PoolStatus::Terminal)],
            true,
        )
        .expect("terminalize drained market");
    fixture
        .send(
            vec![fixture.set_pool_status_instruction(PoolStatus::ClaimsOnly)],
            true,
        )
        .expect("enter claims-only pool mode");
    fixture
        .send(
            vec![fixture.set_pool_status_instruction(PoolStatus::Terminal)],
            true,
        )
        .expect("terminalize drained pool");
    fixture
        .send_controller(vec![fixture.withdraw_idle_cash_instruction(50_000_000)])
        .expect("idle collateral remains recoverable in terminal mode");

    let pool =
        decode_account::<PoolState>(&fixture.svm.get_account(&fixture.pool).unwrap().data).unwrap();
    let market =
        decode_account::<MarketRiskState>(&fixture.svm.get_account(&fixture.market).unwrap().data)
            .unwrap();
    let controller = decode_account::<ControllerRiskState>(
        &fixture
            .svm
            .get_account(&fixture.controller_risk)
            .unwrap()
            .data,
    )
    .unwrap();
    let source_after =
        TokenAccount::unpack(&fixture.svm.get_account(&fixture.source).unwrap().data)
            .unwrap()
            .amount;
    let vault_after = TokenAccount::unpack(&fixture.svm.get_account(&fixture.vault).unwrap().data)
        .unwrap()
        .amount;
    assert_eq!(pool.status, PoolStatus::Terminal as u8);
    assert_eq!(market.status, PoolStatus::Terminal as u8);
    assert_eq!(pool.idle_cash_collateral.get(), 100_000_000);
    assert_eq!(controller.available_cash_collateral.get(), 100_000_000);
    assert_eq!(source_after - source_before, 150_000_000);
    assert_eq!(vault_before - vault_after, 150_000_000);
    assert_eq!(pool.free_cash.get(), 0);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.fee_accrual.get(), 0);
    assert_eq!(
        pool.represented_quote_custody().unwrap(),
        u128::from(vault_after)
    );
}

#[test]
fn real_sbf_wind_down_rejects_open_risk_and_withdrawal_boundary_failures_atomically() {
    let mut fixture = Fixture::new(236);
    bootstrap_cash_open(&mut fixture, false);
    let entry = fixture.install_pyth_price(&PRIMARY_FEED_ID, 100_000_000, 100_000, ORACLE_TIME);
    fixture
        .send_controller(vec![
            fixture.prepare_position_instruction(),
            fixture.open_cash_instruction(entry, None, OPEN_COLLATERAL, OPEN_NOTIONAL, 100),
        ])
        .unwrap();
    fixture
        .send(
            vec![fixture.set_market_status_instruction(PoolStatus::CloseOnly)],
            true,
        )
        .unwrap();
    fixture
        .send(
            vec![fixture.set_pool_status_instruction(PoolStatus::CloseOnly)],
            true,
        )
        .unwrap();

    let keys = [
        fixture.pool,
        fixture.market,
        fixture.controller_risk,
        fixture.vault,
        fixture.source,
        fixture.quote_mint,
    ];
    let before = account_snapshot(&fixture, &keys);
    assert!(fixture
        .send(
            vec![fixture.set_market_status_instruction(PoolStatus::Terminal)],
            true,
        )
        .is_err());
    assert_eq!(account_snapshot(&fixture, &keys), before);
    assert!(fixture
        .send(
            vec![fixture.set_pool_status_instruction(PoolStatus::ClaimsOnly)],
            true,
        )
        .is_err());
    assert_eq!(account_snapshot(&fixture, &keys), before);

    let mut wrong_signer = fixture.withdraw_idle_cash_instruction(1);
    wrong_signer.accounts[0].is_signer = false;
    assert!(fixture.send(vec![wrong_signer], false).is_err());
    assert_eq!(account_snapshot(&fixture, &keys), before);
    assert!(fixture
        .send_controller(vec![fixture.withdraw_idle_cash_instruction(DEPOSIT_AMOUNT)])
        .is_err());
    assert_eq!(account_snapshot(&fixture, &keys), before);

    let mut reverse = fixture.set_pool_status_instruction(PoolStatus::Active);
    reverse.accounts[0] = AccountMeta::new_readonly(fixture.controller.pubkey(), true);
    assert!(fixture.send_controller(vec![reverse]).is_err());
    assert_eq!(account_snapshot(&fixture, &keys), before);
}
