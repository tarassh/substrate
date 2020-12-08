#![feature(prelude_import)]
//! Various implementation for `ElectionProvider`.
//!
//! Two main election providers are implemented in this crate.
//!
//! 1.  [`onchain`]: A `struct` that perform the election onchain (i.e. in the fly). This type is
//!     likely to be expensive for most chains and damage the block time. Only use when you are sure
//!     that the inputs are bounded and small enough.
//! 2. [`two_phase`]: An individual `pallet` that performs the election in two phases, signed and
//!    unsigned. Needless to say, the pallet needs to be included in the final runtime.
#[prelude_import]
use std::prelude::v1::*;
#[macro_use]
extern crate std;
/// The onchain module.
pub mod onchain {
	use sp_arithmetic::PerThing;
	use sp_election_providers::ElectionProvider;
	use sp_npos_elections::{
		ElectionResult, ExtendedBalance, IdentifierT, PerThing128, Supports, VoteWeight,
	};
	use sp_runtime::RuntimeDebug;
	use sp_std::{collections::btree_map::BTreeMap, prelude::*};
	/// Errors of the on-chain election.
	pub enum Error {
		/// An internal error in the NPoS elections crate.
		NposElections(sp_npos_elections::Error),
	}
	impl core::fmt::Debug for Error {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
				Self::NposElections(ref a0) => {
					fmt.debug_tuple("Error::NposElections").field(a0).finish()
				}
				_ => Ok(()),
			}
		}
	}
	impl ::core::marker::StructuralEq for Error {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::Eq for Error {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<sp_npos_elections::Error>;
			}
		}
	}
	impl ::core::marker::StructuralPartialEq for Error {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::PartialEq for Error {
		#[inline]
		fn eq(&self, other: &Error) -> bool {
			match (&*self, &*other) {
				(&Error::NposElections(ref __self_0), &Error::NposElections(ref __arg_1_0)) => {
					(*__self_0) == (*__arg_1_0)
				}
			}
		}
		#[inline]
		fn ne(&self, other: &Error) -> bool {
			match (&*self, &*other) {
				(&Error::NposElections(ref __self_0), &Error::NposElections(ref __arg_1_0)) => {
					(*__self_0) != (*__arg_1_0)
				}
			}
		}
	}
	impl From<sp_npos_elections::Error> for Error {
		fn from(e: sp_npos_elections::Error) -> Self {
			Error::NposElections(e)
		}
	}
	/// A simple on-chian implementation of the election provider trait.
	///
	/// This will accept voting data on the fly and produce the results immediately.
	///
	/// ### Warning
	///
	/// This can be very expensive to run frequently on-chain. Use with care.
	pub struct OnChainSequentialPhragmen;
	impl<AccountId: IdentifierT> ElectionProvider<AccountId> for OnChainSequentialPhragmen {
		type Error = Error;
		const NEEDS_ELECT_DATA: bool = true;
		fn elect<P: PerThing128>(
			to_elect: usize,
			targets: Vec<AccountId>,
			voters: Vec<(AccountId, VoteWeight, Vec<AccountId>)>,
		) -> Result<Supports<AccountId>, Self::Error>
		where
			ExtendedBalance: From<<P as PerThing>::Inner>,
		{
			let mut stake_map: BTreeMap<AccountId, VoteWeight> = BTreeMap::new();
			voters.iter().for_each(|(v, s, _)| {
				stake_map.insert(v.clone(), *s);
			});
			let stake_of = Box::new(|w: &AccountId| -> VoteWeight {
				stake_map.get(w).cloned().unwrap_or_default()
			});
			sp_npos_elections::seq_phragmen::<_, P>(to_elect, targets, voters, None)
				.and_then(|e| {
					let ElectionResult {
						winners,
						assignments,
					} = e;
					let staked = sp_npos_elections::assignment_ratio_to_staked_normalized(
						assignments,
						&stake_of,
					)?;
					let winners = sp_npos_elections::to_without_backing(winners);
					sp_npos_elections::to_supports(&winners, &staked)
				})
				.map_err(From::from)
		}
		fn ongoing() -> bool {
			false
		}
	}
}
/// The two-phase module.
pub mod two_phase {
	//! # Two phase election provider pallet.
	//!
	//! As the name suggests, this election provider has two distinct phases (see [`Phase`]), signed and
	//! unsigned.
	//!
	//! ## Phases
	//!
	//! The timeline of pallet is as follows. At each block,
	//! [`ElectionDataProvider::next_election_prediction`] is used to estimate the time remaining to the
	//! next call to `elect`. Based on this, a phase is chosen. The timeline is as follows.
	//!
	//! ```ignore
	//!                                                                    elect()
	//!                 +   <--T::SignedPhase-->  +  <--T::UnsignedPhase-->   +
	//!   +-------------------------------------------------------------------+
	//!    Phase::Off   +       Phase::Signed     +      Phase::Unsigned      +
	//!
	//! Note that the unsigned phase starts `T::UnsignedPhase` blocks before the
	//! `next_election_prediction`, but only ends when a call to `ElectionProvider::elect` happens.
	//!
	//! ```
	//! ### Signed Phase
	//!
	//!	In the signed phase, solutions (of type [`RawSolution`]) are submitted and queued on chain. A
	//! deposit is reserved, based on the size of the solution, for the cost of keeping this solution
	//! on-chain for a number of blocks. A maximum of [`Trait::MaxSignedSubmissions`] solutions are
	//! stored. The queue is always sorted based on score (worse -> best).
	//!
	//! Upon arrival of a new solution:
	//!
	//! 1. If the queue is not full, it is stored.
	//! 2. If the queue is full but the submitted solution is better than one of the queued ones, the
	//!    worse solution is discarded (TODO: what to do with the bond?) and the new solution is stored
	//!    in the correct index.
	//! 3. If the queue is full and the solution is not an improvement compared to any of the queued
	//!    ones, it is instantly rejected and no additional bond is reserved.
	//!
	//! A signed solution cannot be reversed, taken back, updated, or retracted. In other words, the
	//! origin can not bail out in any way.
	//!
	//! Upon the end of the signed phase, the solutions are examined from worse to best (i.e. `pop()`ed
	//! until drained). Each solution undergoes an expensive [`Module::feasibility_check`], which ensure
	//! the score claimed by this score was correct, among other checks. At each step, if the current
	//! best solution is passes the feasibility check, it is considered to be the best one. The sender
	//! of the origin is rewarded, and the rest of the queued solutions get their deposit back, without
	//! being checked.
	//!
	//! The following example covers all of the cases at the end of the signed phase:
	//!
	//! ```ignore
	//! Queue
	//! +-------------------------------+
	//! |Solution(score=20, valid=false)| +-->  Slashed
	//! +-------------------------------+
	//! |Solution(score=15, valid=true )| +-->  Rewarded
	//! +-------------------------------+
	//! |Solution(score=10, valid=true )| +-->  Discarded
	//! +-------------------------------+
	//! |Solution(score=05, valid=false)| +-->  Discarded
	//! +-------------------------------+
	//! |             None              |
	//! +-------------------------------+
	//! ```
	//!
	//! TODO: what if length of some phase is zero?
	//!
	//! Note that both of the bottom solutions end up being discarded and get their deposit back,
	//! despite one of them being invalid.
	//!
	//! ## Unsigned Phase
	//!
	//! If signed phase ends with a good solution, then the unsigned phase will be `active`
	//! ([`Phase::Unsigned(true)`]), else the unsigned phase will be `passive`.
	//!
	//! TODO
	//!
	//! ### Fallback
	//!
	//! If we reach the end of both phases (i.e. call to `ElectionProvider::elect` happens) and no good
	//! solution is queued, then we fallback to an on-chain election. The on-chain election is slow, and
	//! contains to balancing or reduction post-processing.
	//!
	//! ## Correct Submission
	//!
	//! TODO
	//!
	//! ## Accuracy
	//!
	//! TODO
	//!
	use crate::onchain::OnChainSequentialPhragmen;
	use codec::{Decode, Encode, HasCompact};
	use frame_support::{
		decl_event, decl_module, decl_storage,
		dispatch::DispatchResultWithPostInfo,
		ensure,
		traits::{Currency, Get, OnUnbalanced, ReservableCurrency},
		weights::Weight,
	};
	use frame_system::{ensure_none, ensure_signed, offchain::SendTransactionTypes};
	use sp_election_providers::{ElectionDataProvider, ElectionProvider};
	use sp_npos_elections::{
		assignment_ratio_to_staked_normalized, is_score_better, Assignment, CompactSolution,
		ElectionScore, EvaluateSupport, ExtendedBalance, PerThing128, Supports, VoteWeight,
	};
	use sp_runtime::{
		traits::Zero, transaction_validity::TransactionPriority, InnerOf, PerThing, Perbill,
		RuntimeDebug,
	};
	use sp_std::prelude::*;
	#[cfg(any(feature = "runtime-benchmarks", test))]
	pub mod benchmarking {
		//! Two phase election pallet benchmarking.
		use super::*;
		use crate::two_phase::{Module as TwoPhase, *};
		pub use frame_benchmarking::{account, benchmarks, whitelist_account, whitelisted_caller};
		use frame_support::assert_ok;
		use rand::{seq::SliceRandom, thread_rng};
		use sp_npos_elections::{ExtendedBalance, VoteWeight};
		use sp_runtime::InnerOf;
		const SEED: u32 = 0;
		/// Generate mock on-chain snapshots.
		///
		/// This emulates the start of signed phase, where snapshots are received from an upstream crate.
		fn mock_snapshot<T: Trait>(
			witness: WitnessData,
		) -> (
			Vec<T::AccountId>,
			Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>,
		)
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			let targets: Vec<T::AccountId> = (0..witness.targets)
				.map(|i| account("Targets", i, SEED))
				.collect();
			let mut voters = (0..(witness.voters - witness.targets))
				.map(|i| {
					let mut rng = thread_rng();
					let stake = 1000_000u64;
					let to_vote = rand::random::<usize>() % <CompactOf<T>>::LIMIT + 1;
					let votes = targets
						.as_slice()
						.choose_multiple(&mut rng, to_vote)
						.cloned()
						.collect::<Vec<_>>();
					let voter = account::<T::AccountId>("Voter", i, SEED);
					(voter, stake, votes)
				})
				.collect::<Vec<_>>();
			voters.extend(
				targets
					.iter()
					.map(|t| (t.clone(), 1000_000_0u64, <[_]>::into_vec(box [t.clone()]))),
			);
			(targets, voters)
		}
		fn put_mock_snapshot<T: Trait>(witness: WitnessData, desired_targets: u32)
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			let (targets, voters) = mock_snapshot::<T>(witness);
			<SnapshotTargets<T>>::put(targets);
			<SnapshotVoters<T>>::put(voters);
			DesiredTargets::put(desired_targets);
		}
		#[allow(non_camel_case_types)]
		struct submit_signed;
		#[allow(unused_variables)]
		impl<T: Trait> ::frame_benchmarking::BenchmarkingSetup<T> for submit_signed
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			fn components(&self) -> Vec<(::frame_benchmarking::BenchmarkParameter, u32, u32)> {
				::alloc::vec::Vec::new()
			}
			fn instance(
				&self,
				components: &[(::frame_benchmarking::BenchmarkParameter, u32)],
				verify: bool,
			) -> Result<Box<dyn FnOnce() -> Result<(), &'static str>>, &'static str> {
				Ok(Box::new(move || -> Result<(), &'static str> {
					{};
					if verify {
						{};
					}
					Ok(())
				}))
			}
		}
		fn test_benchmark_submit_signed<T: Trait>() -> Result<(), &'static str>
		where
			T: frame_system::Trait,
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			let selected_benchmark = SelectedBenchmark::submit_signed;
			let components =
				<SelectedBenchmark as ::frame_benchmarking::BenchmarkingSetup<T, _>>::components(
					&selected_benchmark,
				);
			let execute_benchmark = | c : Vec < ( :: frame_benchmarking :: BenchmarkParameter , u32 ) > | -> Result < ( ) , & 'static str > { let closure_to_verify = < SelectedBenchmark as :: frame_benchmarking :: BenchmarkingSetup < T , _ > > :: instance ( & selected_benchmark , & c , true ) ? ; if :: frame_benchmarking :: Zero :: is_zero ( & frame_system :: Module :: < T > :: block_number ( ) ) { frame_system :: Module :: < T > :: set_block_number ( 1 . into ( ) ) ; } closure_to_verify ( ) ? ; :: frame_benchmarking :: benchmarking :: wipe_db ( ) ; Ok ( ( ) ) } ;
			if components.is_empty() {
				execute_benchmark(Default::default())?;
			} else {
				for (_, (name, low, high)) in components.iter().enumerate() {
					for component_value in <[_]>::into_vec(box [low, high]) {
						let c: Vec<(::frame_benchmarking::BenchmarkParameter, u32)> = components
							.iter()
							.enumerate()
							.map(|(_, (n, _, h))| {
								if n == name {
									(*n, *component_value)
								} else {
									(*n, *h)
								}
							})
							.collect();
						execute_benchmark(c)?;
					}
				}
			}
			Ok(())
		}
		#[allow(non_camel_case_types)]
		struct submit_unsigned;
		#[allow(unused_variables)]
		impl<T: Trait> ::frame_benchmarking::BenchmarkingSetup<T> for submit_unsigned
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			fn components(&self) -> Vec<(::frame_benchmarking::BenchmarkParameter, u32, u32)> {
				::alloc::vec::Vec::new()
			}
			fn instance(
				&self,
				components: &[(::frame_benchmarking::BenchmarkParameter, u32)],
				verify: bool,
			) -> Result<Box<dyn FnOnce() -> Result<(), &'static str>>, &'static str> {
				Ok(Box::new(move || -> Result<(), &'static str> {
					{};
					if verify {
						{};
					}
					Ok(())
				}))
			}
		}
		fn test_benchmark_submit_unsigned<T: Trait>() -> Result<(), &'static str>
		where
			T: frame_system::Trait,
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			let selected_benchmark = SelectedBenchmark::submit_unsigned;
			let components =
				<SelectedBenchmark as ::frame_benchmarking::BenchmarkingSetup<T, _>>::components(
					&selected_benchmark,
				);
			let execute_benchmark = | c : Vec < ( :: frame_benchmarking :: BenchmarkParameter , u32 ) > | -> Result < ( ) , & 'static str > { let closure_to_verify = < SelectedBenchmark as :: frame_benchmarking :: BenchmarkingSetup < T , _ > > :: instance ( & selected_benchmark , & c , true ) ? ; if :: frame_benchmarking :: Zero :: is_zero ( & frame_system :: Module :: < T > :: block_number ( ) ) { frame_system :: Module :: < T > :: set_block_number ( 1 . into ( ) ) ; } closure_to_verify ( ) ? ; :: frame_benchmarking :: benchmarking :: wipe_db ( ) ; Ok ( ( ) ) } ;
			if components.is_empty() {
				execute_benchmark(Default::default())?;
			} else {
				for (_, (name, low, high)) in components.iter().enumerate() {
					for component_value in <[_]>::into_vec(box [low, high]) {
						let c: Vec<(::frame_benchmarking::BenchmarkParameter, u32)> = components
							.iter()
							.enumerate()
							.map(|(_, (n, _, h))| {
								if n == name {
									(*n, *component_value)
								} else {
									(*n, *h)
								}
							})
							.collect();
						execute_benchmark(c)?;
					}
				}
			}
			Ok(())
		}
		#[allow(non_camel_case_types)]
		struct open_signed_phase;
		#[allow(unused_variables)]
		impl<T: Trait> ::frame_benchmarking::BenchmarkingSetup<T> for open_signed_phase
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			fn components(&self) -> Vec<(::frame_benchmarking::BenchmarkParameter, u32, u32)> {
				::alloc::vec::Vec::new()
			}
			fn instance(
				&self,
				components: &[(::frame_benchmarking::BenchmarkParameter, u32)],
				verify: bool,
			) -> Result<Box<dyn FnOnce() -> Result<(), &'static str>>, &'static str> {
				Ok(Box::new(move || -> Result<(), &'static str> {
					{};
					if verify {
						{};
					}
					Ok(())
				}))
			}
		}
		fn test_benchmark_open_signed_phase<T: Trait>() -> Result<(), &'static str>
		where
			T: frame_system::Trait,
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			let selected_benchmark = SelectedBenchmark::open_signed_phase;
			let components =
				<SelectedBenchmark as ::frame_benchmarking::BenchmarkingSetup<T, _>>::components(
					&selected_benchmark,
				);
			let execute_benchmark = | c : Vec < ( :: frame_benchmarking :: BenchmarkParameter , u32 ) > | -> Result < ( ) , & 'static str > { let closure_to_verify = < SelectedBenchmark as :: frame_benchmarking :: BenchmarkingSetup < T , _ > > :: instance ( & selected_benchmark , & c , true ) ? ; if :: frame_benchmarking :: Zero :: is_zero ( & frame_system :: Module :: < T > :: block_number ( ) ) { frame_system :: Module :: < T > :: set_block_number ( 1 . into ( ) ) ; } closure_to_verify ( ) ? ; :: frame_benchmarking :: benchmarking :: wipe_db ( ) ; Ok ( ( ) ) } ;
			if components.is_empty() {
				execute_benchmark(Default::default())?;
			} else {
				for (_, (name, low, high)) in components.iter().enumerate() {
					for component_value in <[_]>::into_vec(box [low, high]) {
						let c: Vec<(::frame_benchmarking::BenchmarkParameter, u32)> = components
							.iter()
							.enumerate()
							.map(|(_, (n, _, h))| {
								if n == name {
									(*n, *component_value)
								} else {
									(*n, *h)
								}
							})
							.collect();
						execute_benchmark(c)?;
					}
				}
			}
			Ok(())
		}
		#[allow(non_camel_case_types)]
		struct close_signed_phase;
		#[allow(unused_variables)]
		impl<T: Trait> ::frame_benchmarking::BenchmarkingSetup<T> for close_signed_phase
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			fn components(&self) -> Vec<(::frame_benchmarking::BenchmarkParameter, u32, u32)> {
				::alloc::vec::Vec::new()
			}
			fn instance(
				&self,
				components: &[(::frame_benchmarking::BenchmarkParameter, u32)],
				verify: bool,
			) -> Result<Box<dyn FnOnce() -> Result<(), &'static str>>, &'static str> {
				Ok(Box::new(move || -> Result<(), &'static str> {
					{};
					if verify {
						{};
					}
					Ok(())
				}))
			}
		}
		fn test_benchmark_close_signed_phase<T: Trait>() -> Result<(), &'static str>
		where
			T: frame_system::Trait,
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			let selected_benchmark = SelectedBenchmark::close_signed_phase;
			let components =
				<SelectedBenchmark as ::frame_benchmarking::BenchmarkingSetup<T, _>>::components(
					&selected_benchmark,
				);
			let execute_benchmark = | c : Vec < ( :: frame_benchmarking :: BenchmarkParameter , u32 ) > | -> Result < ( ) , & 'static str > { let closure_to_verify = < SelectedBenchmark as :: frame_benchmarking :: BenchmarkingSetup < T , _ > > :: instance ( & selected_benchmark , & c , true ) ? ; if :: frame_benchmarking :: Zero :: is_zero ( & frame_system :: Module :: < T > :: block_number ( ) ) { frame_system :: Module :: < T > :: set_block_number ( 1 . into ( ) ) ; } closure_to_verify ( ) ? ; :: frame_benchmarking :: benchmarking :: wipe_db ( ) ; Ok ( ( ) ) } ;
			if components.is_empty() {
				execute_benchmark(Default::default())?;
			} else {
				for (_, (name, low, high)) in components.iter().enumerate() {
					for component_value in <[_]>::into_vec(box [low, high]) {
						let c: Vec<(::frame_benchmarking::BenchmarkParameter, u32)> = components
							.iter()
							.enumerate()
							.map(|(_, (n, _, h))| {
								if n == name {
									(*n, *component_value)
								} else {
									(*n, *h)
								}
							})
							.collect();
						execute_benchmark(c)?;
					}
				}
			}
			Ok(())
		}
		#[allow(non_camel_case_types)]
		struct feasibility_check;
		#[allow(unused_variables)]
		impl<T: Trait> ::frame_benchmarking::BenchmarkingSetup<T> for feasibility_check
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			fn components(&self) -> Vec<(::frame_benchmarking::BenchmarkParameter, u32, u32)> {
				<[_]>::into_vec(box [
					(::frame_benchmarking::BenchmarkParameter::v, 200, 300),
					(::frame_benchmarking::BenchmarkParameter::t, 50, 80),
					(::frame_benchmarking::BenchmarkParameter::a, 20, 80),
					(::frame_benchmarking::BenchmarkParameter::d, 20, 40),
				])
			}
			fn instance(
				&self,
				components: &[(::frame_benchmarking::BenchmarkParameter, u32)],
				verify: bool,
			) -> Result<Box<dyn FnOnce() -> Result<(), &'static str>>, &'static str> {
				let v = components
					.iter()
					.find(|&c| c.0 == ::frame_benchmarking::BenchmarkParameter::v)
					.ok_or("Could not find component in benchmark preparation.")?
					.1;
				let t = components
					.iter()
					.find(|&c| c.0 == ::frame_benchmarking::BenchmarkParameter::t)
					.ok_or("Could not find component in benchmark preparation.")?
					.1;
				let a = components
					.iter()
					.find(|&c| c.0 == ::frame_benchmarking::BenchmarkParameter::a)
					.ok_or("Could not find component in benchmark preparation.")?
					.1;
				let d = components
					.iter()
					.find(|&c| c.0 == ::frame_benchmarking::BenchmarkParameter::d)
					.ok_or("Could not find component in benchmark preparation.")?
					.1;
				();
				();
				();
				();
				{
					::std::io::_print(::core::fmt::Arguments::new_v1(
						&["running v  ", ", t ", ", a ", ", d ", "\n"],
						&match (&v, &t, &a, &d) {
							(arg0, arg1, arg2, arg3) => [
								::core::fmt::ArgumentV1::new(arg0, ::core::fmt::Display::fmt),
								::core::fmt::ArgumentV1::new(arg1, ::core::fmt::Display::fmt),
								::core::fmt::ArgumentV1::new(arg2, ::core::fmt::Display::fmt),
								::core::fmt::ArgumentV1::new(arg3, ::core::fmt::Display::fmt),
							],
						},
					));
				};
				let witness = WitnessData {
					voters: v,
					targets: t,
				};
				put_mock_snapshot::<T>(witness, d);
				let voters = <TwoPhase<T>>::snapshot_voters().unwrap();
				let targets = <TwoPhase<T>>::snapshot_targets().unwrap();
				let voter_index =
					|who: &T::AccountId| -> Option<crate::two_phase::CompactVoterIndexOf<T>> {
						voters . iter ( ) . position ( | ( x , _ , _ ) | x == who ) . and_then ( | i | < usize as crate :: TryInto < crate :: two_phase :: CompactVoterIndexOf < T > > > :: try_into ( i ) . ok ( ) )
					};
				let voter_at =
					|i: crate::two_phase::CompactVoterIndexOf<T>| -> Option<T::AccountId> {
						< crate :: two_phase :: CompactVoterIndexOf < T > as crate :: TryInto < usize > > :: try_into ( i ) . ok ( ) . and_then ( | i | voters . get ( i ) . map ( | ( x , _ , _ ) | x ) . cloned ( ) )
					};
				let target_at =
					|i: crate::two_phase::CompactTargetIndexOf<T>| -> Option<T::AccountId> {
						< crate :: two_phase :: CompactTargetIndexOf < T > as crate :: TryInto < usize > > :: try_into ( i ) . ok ( ) . and_then ( | i | targets . get ( i ) . cloned ( ) )
					};
				let stake_of = |who: &T::AccountId| -> crate::VoteWeight {
					voters
						.iter()
						.find(|(x, _, _)| x == who)
						.map(|(_, x, _)| *x)
						.unwrap_or_default()
				};
				let RawSolution { compact, score: _ } = <TwoPhase<T>>::mine_solution(0).unwrap();
				let compact = <TwoPhase<T>>::trim_compact(a, compact, voter_index).unwrap();
				{
					match (&(compact.len() as u32), &a) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				{
					match (&(compact.unique_targets().len() as u32), &d) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				let winners = compact
					.unique_targets()
					.iter()
					.map(|i| target_at(*i).unwrap())
					.collect::<Vec<_>>();
				let score = compact
					.clone()
					.score(&winners, stake_of, voter_at, target_at)
					.unwrap();
				let raw_solution = RawSolution { compact, score };
				let compute = ElectionCompute::Unsigned;
				Ok(Box::new(move || -> Result<(), &'static str> {
					{
						let is = <TwoPhase<T>>::feasibility_check(raw_solution, compute);
						match is {
							Ok(_) => (),
							_ => {
								if !false {
									{
										::std::rt::begin_panic_fmt(
											&::core::fmt::Arguments::new_v1_formatted(
												&["Expected Ok(_). Got "],
												&match (&is,) {
													(arg0,) => [::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													)],
												},
												&[::core::fmt::rt::v1::Argument {
													position: 0usize,
													format: ::core::fmt::rt::v1::FormatSpec {
														fill: ' ',
														align:
															::core::fmt::rt::v1::Alignment::Unknown,
														flags: 4u32,
														precision:
															::core::fmt::rt::v1::Count::Implied,
														width: ::core::fmt::rt::v1::Count::Implied,
													},
												}],
											),
										)
									}
								}
							}
						};
					};
					if verify {
						{};
					}
					Ok(())
				}))
			}
		}
		fn test_benchmark_feasibility_check<T: Trait>() -> Result<(), &'static str>
		where
			T: frame_system::Trait,
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			let selected_benchmark = SelectedBenchmark::feasibility_check;
			let components =
				<SelectedBenchmark as ::frame_benchmarking::BenchmarkingSetup<T, _>>::components(
					&selected_benchmark,
				);
			let execute_benchmark = | c : Vec < ( :: frame_benchmarking :: BenchmarkParameter , u32 ) > | -> Result < ( ) , & 'static str > { let closure_to_verify = < SelectedBenchmark as :: frame_benchmarking :: BenchmarkingSetup < T , _ > > :: instance ( & selected_benchmark , & c , true ) ? ; if :: frame_benchmarking :: Zero :: is_zero ( & frame_system :: Module :: < T > :: block_number ( ) ) { frame_system :: Module :: < T > :: set_block_number ( 1 . into ( ) ) ; } closure_to_verify ( ) ? ; :: frame_benchmarking :: benchmarking :: wipe_db ( ) ; Ok ( ( ) ) } ;
			if components.is_empty() {
				execute_benchmark(Default::default())?;
			} else {
				for (_, (name, low, high)) in components.iter().enumerate() {
					for component_value in <[_]>::into_vec(box [low, high]) {
						let c: Vec<(::frame_benchmarking::BenchmarkParameter, u32)> = components
							.iter()
							.enumerate()
							.map(|(_, (n, _, h))| {
								if n == name {
									(*n, *component_value)
								} else {
									(*n, *h)
								}
							})
							.collect();
						execute_benchmark(c)?;
					}
				}
			}
			Ok(())
		}
		#[allow(non_camel_case_types)]
		enum SelectedBenchmark {
			submit_signed,
			submit_unsigned,
			open_signed_phase,
			close_signed_phase,
			feasibility_check,
		}
		impl<T: Trait> ::frame_benchmarking::BenchmarkingSetup<T> for SelectedBenchmark
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			fn components(&self) -> Vec<(::frame_benchmarking::BenchmarkParameter, u32, u32)> {
				match self { Self :: submit_signed => < submit_signed as :: frame_benchmarking :: BenchmarkingSetup < T > > :: components ( & submit_signed ) , Self :: submit_unsigned => < submit_unsigned as :: frame_benchmarking :: BenchmarkingSetup < T > > :: components ( & submit_unsigned ) , Self :: open_signed_phase => < open_signed_phase as :: frame_benchmarking :: BenchmarkingSetup < T > > :: components ( & open_signed_phase ) , Self :: close_signed_phase => < close_signed_phase as :: frame_benchmarking :: BenchmarkingSetup < T > > :: components ( & close_signed_phase ) , Self :: feasibility_check => < feasibility_check as :: frame_benchmarking :: BenchmarkingSetup < T > > :: components ( & feasibility_check ) , }
			}
			fn instance(
				&self,
				components: &[(::frame_benchmarking::BenchmarkParameter, u32)],
				verify: bool,
			) -> Result<Box<dyn FnOnce() -> Result<(), &'static str>>, &'static str> {
				match self {
					Self::submit_signed => {
						<submit_signed as ::frame_benchmarking::BenchmarkingSetup<T>>::instance(
							&submit_signed,
							components,
							verify,
						)
					}
					Self::submit_unsigned => {
						<submit_unsigned as ::frame_benchmarking::BenchmarkingSetup<T>>::instance(
							&submit_unsigned,
							components,
							verify,
						)
					}
					Self::open_signed_phase => {
						<open_signed_phase as ::frame_benchmarking::BenchmarkingSetup<T>>::instance(
							&open_signed_phase,
							components,
							verify,
						)
					}
					Self::close_signed_phase => {
						<close_signed_phase as ::frame_benchmarking::BenchmarkingSetup<T>>::instance(
							&close_signed_phase,
							components,
							verify,
						)
					}
					Self::feasibility_check => {
						<feasibility_check as ::frame_benchmarking::BenchmarkingSetup<T>>::instance(
							&feasibility_check,
							components,
							verify,
						)
					}
				}
			}
		}
		impl<T: Trait> ::frame_benchmarking::Benchmarking<::frame_benchmarking::BenchmarkResults>
			for Module<T>
		where
			T: frame_system::Trait,
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			fn benchmarks(extra: bool) -> Vec<&'static [u8]> {
				let mut all = <[_]>::into_vec(box [
					"submit_signed".as_ref(),
					"submit_unsigned".as_ref(),
					"open_signed_phase".as_ref(),
					"close_signed_phase".as_ref(),
					"feasibility_check".as_ref(),
				]);
				if !extra {
					let extra = [];
					all.retain(|x| !extra.contains(x));
				}
				all
			}
			fn run_benchmark(
				extrinsic: &[u8],
				lowest_range_values: &[u32],
				highest_range_values: &[u32],
				steps: &[u32],
				repeat: u32,
				whitelist: &[::frame_benchmarking::TrackedStorageKey],
				verify: bool,
			) -> Result<Vec<::frame_benchmarking::BenchmarkResults>, &'static str> {
				let extrinsic = sp_std::str::from_utf8(extrinsic)
					.map_err(|_| "`extrinsic` is not a valid utf8 string!")?;
				let selected_benchmark = match extrinsic {
					"submit_signed" => SelectedBenchmark::submit_signed,
					"submit_unsigned" => SelectedBenchmark::submit_unsigned,
					"open_signed_phase" => SelectedBenchmark::open_signed_phase,
					"close_signed_phase" => SelectedBenchmark::close_signed_phase,
					"feasibility_check" => SelectedBenchmark::feasibility_check,
					_ => return Err("Could not find extrinsic."),
				};
				let mut results: Vec<::frame_benchmarking::BenchmarkResults> = Vec::new();
				if repeat == 0 {
					return Ok(results);
				}
				let mut whitelist = whitelist.to_vec();
				let whitelisted_caller_key = < frame_system :: Account < T > as frame_support :: storage :: StorageMap < _ , _ > > :: hashed_key_for ( :: frame_benchmarking :: whitelisted_caller :: < T :: AccountId > ( ) ) ;
				whitelist.push(whitelisted_caller_key.into());
				::frame_benchmarking::benchmarking::set_whitelist(whitelist);
				::frame_benchmarking::benchmarking::commit_db();
				::frame_benchmarking::benchmarking::wipe_db();
				let components = <SelectedBenchmark as ::frame_benchmarking::BenchmarkingSetup<
					T,
				>>::components(&selected_benchmark);
				let mut prev_steps = 10;
				let repeat_benchmark = |repeat: u32,
				                        c: &[(::frame_benchmarking::BenchmarkParameter, u32)],
				                        results: &mut Vec<
					::frame_benchmarking::BenchmarkResults,
				>,
				                        verify: bool|
				 -> Result<(), &'static str> {
					for _ in 0..repeat {
						let closure_to_benchmark = < SelectedBenchmark as :: frame_benchmarking :: BenchmarkingSetup < T > > :: instance ( & selected_benchmark , c , verify ) ? ;
						if ::frame_benchmarking::Zero::is_zero(
							&frame_system::Module::<T>::block_number(),
						) {
							frame_system::Module::<T>::set_block_number(1.into());
						}
						::frame_benchmarking::benchmarking::commit_db();
						::frame_benchmarking::benchmarking::reset_read_write_count();
						if verify {
							closure_to_benchmark()?;
						} else {
							{
								let lvl = ::log::Level::Trace;
								if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
									:: log :: __private_api_log ( :: core :: fmt :: Arguments :: new_v1 ( & [ "Start Benchmark: " ] , & match ( & c , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } ) , lvl , & ( "benchmark" , "frame_election_providers::two_phase::benchmarking" , "frame/election-providers/src/two_phase/benchmarking.rs" , 81u32 ) ) ;
								}
							};
							let start_extrinsic =
								::frame_benchmarking::benchmarking::current_time();
							closure_to_benchmark()?;
							let finish_extrinsic =
								::frame_benchmarking::benchmarking::current_time();
							let elapsed_extrinsic = finish_extrinsic - start_extrinsic;
							::frame_benchmarking::benchmarking::commit_db();
							{
								let lvl = ::log::Level::Trace;
								if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
									:: log :: __private_api_log ( :: core :: fmt :: Arguments :: new_v1 ( & [ "End Benchmark: " , " ns" ] , & match ( & elapsed_extrinsic , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Display :: fmt ) ] , } ) , lvl , & ( "benchmark" , "frame_election_providers::two_phase::benchmarking" , "frame/election-providers/src/two_phase/benchmarking.rs" , 81u32 ) ) ;
								}
							};
							let read_write_count =
								::frame_benchmarking::benchmarking::read_write_count();
							{
								let lvl = ::log::Level::Trace;
								if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
									:: log :: __private_api_log ( :: core :: fmt :: Arguments :: new_v1 ( & [ "Read/Write Count " ] , & match ( & read_write_count , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } ) , lvl , & ( "benchmark" , "frame_election_providers::two_phase::benchmarking" , "frame/election-providers/src/two_phase/benchmarking.rs" , 81u32 ) ) ;
								}
							};
							let start_storage_root =
								::frame_benchmarking::benchmarking::current_time();
							::frame_benchmarking::storage_root();
							let finish_storage_root =
								::frame_benchmarking::benchmarking::current_time();
							let elapsed_storage_root = finish_storage_root - start_storage_root;
							results.push(::frame_benchmarking::BenchmarkResults {
								components: c.to_vec(),
								extrinsic_time: elapsed_extrinsic,
								storage_root_time: elapsed_storage_root,
								reads: read_write_count.0,
								repeat_reads: read_write_count.1,
								writes: read_write_count.2,
								repeat_writes: read_write_count.3,
							});
						}
						::frame_benchmarking::benchmarking::wipe_db();
					}
					Ok(())
				};
				if components.is_empty() {
					if verify {
						repeat_benchmark(1, Default::default(), &mut Vec::new(), true)?;
					}
					repeat_benchmark(repeat, Default::default(), &mut results, false)?;
				} else {
					for (idx, (name, low, high)) in components.iter().enumerate() {
						let steps = steps.get(idx).cloned().unwrap_or(prev_steps);
						prev_steps = steps;
						if steps == 0 {
							continue;
						}
						let lowest = lowest_range_values.get(idx).cloned().unwrap_or(*low);
						let highest = highest_range_values.get(idx).cloned().unwrap_or(*high);
						let diff = highest - lowest;
						let step_size = (diff / steps).max(1);
						let num_of_steps = diff / step_size + 1;
						for s in 0..num_of_steps {
							let component_value = lowest + step_size * s;
							let c: Vec<(::frame_benchmarking::BenchmarkParameter, u32)> =
								components
									.iter()
									.enumerate()
									.map(|(idx, (n, _, h))| {
										if n == name {
											(*n, component_value)
										} else {
											(*n, *highest_range_values.get(idx).unwrap_or(h))
										}
									})
									.collect();
							if verify {
								repeat_benchmark(1, &c, &mut Vec::new(), true)?;
							}
							repeat_benchmark(repeat, &c, &mut results, false)?;
						}
					}
				}
				return Ok(results);
			}
		}
		#[cfg(test)]
		mod test {
			use super::*;
			use crate::two_phase::mock::*;
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const test_benchmarks: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName("two_phase::benchmarking::test::test_benchmarks"),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| test::assert_test_result(test_benchmarks())),
			};
			fn test_benchmarks() {
				ExtBuilder::default().build_and_execute(|| {
					let is = test_benchmark_feasibility_check::<Runtime>();
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
				})
			}
		}
	}
	#[cfg(test)]
	mod mock {
		use super::*;
		pub use frame_support::{assert_noop, assert_ok};
		use frame_support::{parameter_types, traits::OnInitialize};
		use parking_lot::RwLock;
		use sp_core::{
			offchain::{
				testing::{PoolState, TestOffchainExt, TestTransactionPoolExt},
				OffchainExt, TransactionPoolExt,
			},
			H256,
		};
		use sp_election_providers::ElectionDataProvider;
		use sp_npos_elections::{
			assignment_ratio_to_staked_normalized, seq_phragmen, to_supports, to_without_backing,
			CompactSolution, ElectionResult, EvaluateSupport,
		};
		use sp_runtime::{
			testing::Header,
			traits::{BlakeTwo256, IdentityLookup},
			PerU16,
		};
		use std::{cell::RefCell, sync::Arc};
		pub struct Runtime;
		impl ::core::marker::StructuralEq for Runtime {}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::cmp::Eq for Runtime {
			#[inline]
			#[doc(hidden)]
			fn assert_receiver_is_total_eq(&self) -> () {
				{}
			}
		}
		impl ::core::marker::StructuralPartialEq for Runtime {}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::cmp::PartialEq for Runtime {
			#[inline]
			fn eq(&self, other: &Runtime) -> bool {
				match *other {
					Runtime => match *self {
						Runtime => true,
					},
				}
			}
		}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::clone::Clone for Runtime {
			#[inline]
			fn clone(&self) -> Runtime {
				match *self {
					Runtime => Runtime,
				}
			}
		}
		pub(crate) type Balances = pallet_balances::Module<Runtime>;
		pub(crate) type System = frame_system::Module<Runtime>;
		pub(crate) type TwoPhase = super::Module<Runtime>;
		pub(crate) type Balance = u64;
		pub(crate) type AccountId = u64;
		extern crate sp_npos_elections as _npos;
		/// A struct to encode a election assignment in a compact way.
		impl _npos::codec::Encode for TestCompact {
			fn encode(&self) -> Vec<u8> {
				let mut r = ::alloc::vec::Vec::new();
				let votes1 = self
					.votes1
					.iter()
					.map(|(v, t)| {
						(
							_npos::codec::Compact(v.clone()),
							_npos::codec::Compact(t.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes1.encode_to(&mut r);
				let votes2 = self
					.votes2
					.iter()
					.map(|(v, (t1, w), t2)| {
						(
							_npos::codec::Compact(v.clone()),
							(
								_npos::codec::Compact(t1.clone()),
								_npos::codec::Compact(w.clone()),
							),
							_npos::codec::Compact(t2.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes2.encode_to(&mut r);
				let votes3 = self
					.votes3
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes3.encode_to(&mut r);
				let votes4 = self
					.votes4
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes4.encode_to(&mut r);
				let votes5 = self
					.votes5
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes5.encode_to(&mut r);
				let votes6 = self
					.votes6
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes6.encode_to(&mut r);
				let votes7 = self
					.votes7
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[5usize].0.clone()),
									_npos::codec::Compact(inner[5usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes7.encode_to(&mut r);
				let votes8 = self
					.votes8
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[5usize].0.clone()),
									_npos::codec::Compact(inner[5usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[6usize].0.clone()),
									_npos::codec::Compact(inner[6usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes8.encode_to(&mut r);
				let votes9 = self
					.votes9
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[5usize].0.clone()),
									_npos::codec::Compact(inner[5usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[6usize].0.clone()),
									_npos::codec::Compact(inner[6usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[7usize].0.clone()),
									_npos::codec::Compact(inner[7usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes9.encode_to(&mut r);
				let votes10 = self
					.votes10
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[5usize].0.clone()),
									_npos::codec::Compact(inner[5usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[6usize].0.clone()),
									_npos::codec::Compact(inner[6usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[7usize].0.clone()),
									_npos::codec::Compact(inner[7usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[8usize].0.clone()),
									_npos::codec::Compact(inner[8usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes10.encode_to(&mut r);
				let votes11 = self
					.votes11
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[5usize].0.clone()),
									_npos::codec::Compact(inner[5usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[6usize].0.clone()),
									_npos::codec::Compact(inner[6usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[7usize].0.clone()),
									_npos::codec::Compact(inner[7usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[8usize].0.clone()),
									_npos::codec::Compact(inner[8usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[9usize].0.clone()),
									_npos::codec::Compact(inner[9usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes11.encode_to(&mut r);
				let votes12 = self
					.votes12
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[5usize].0.clone()),
									_npos::codec::Compact(inner[5usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[6usize].0.clone()),
									_npos::codec::Compact(inner[6usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[7usize].0.clone()),
									_npos::codec::Compact(inner[7usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[8usize].0.clone()),
									_npos::codec::Compact(inner[8usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[9usize].0.clone()),
									_npos::codec::Compact(inner[9usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[10usize].0.clone()),
									_npos::codec::Compact(inner[10usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes12.encode_to(&mut r);
				let votes13 = self
					.votes13
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[5usize].0.clone()),
									_npos::codec::Compact(inner[5usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[6usize].0.clone()),
									_npos::codec::Compact(inner[6usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[7usize].0.clone()),
									_npos::codec::Compact(inner[7usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[8usize].0.clone()),
									_npos::codec::Compact(inner[8usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[9usize].0.clone()),
									_npos::codec::Compact(inner[9usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[10usize].0.clone()),
									_npos::codec::Compact(inner[10usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[11usize].0.clone()),
									_npos::codec::Compact(inner[11usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes13.encode_to(&mut r);
				let votes14 = self
					.votes14
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[5usize].0.clone()),
									_npos::codec::Compact(inner[5usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[6usize].0.clone()),
									_npos::codec::Compact(inner[6usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[7usize].0.clone()),
									_npos::codec::Compact(inner[7usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[8usize].0.clone()),
									_npos::codec::Compact(inner[8usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[9usize].0.clone()),
									_npos::codec::Compact(inner[9usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[10usize].0.clone()),
									_npos::codec::Compact(inner[10usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[11usize].0.clone()),
									_npos::codec::Compact(inner[11usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[12usize].0.clone()),
									_npos::codec::Compact(inner[12usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes14.encode_to(&mut r);
				let votes15 = self
					.votes15
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[5usize].0.clone()),
									_npos::codec::Compact(inner[5usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[6usize].0.clone()),
									_npos::codec::Compact(inner[6usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[7usize].0.clone()),
									_npos::codec::Compact(inner[7usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[8usize].0.clone()),
									_npos::codec::Compact(inner[8usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[9usize].0.clone()),
									_npos::codec::Compact(inner[9usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[10usize].0.clone()),
									_npos::codec::Compact(inner[10usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[11usize].0.clone()),
									_npos::codec::Compact(inner[11usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[12usize].0.clone()),
									_npos::codec::Compact(inner[12usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[13usize].0.clone()),
									_npos::codec::Compact(inner[13usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes15.encode_to(&mut r);
				let votes16 = self
					.votes16
					.iter()
					.map(|(v, inner, t_last)| {
						(
							_npos::codec::Compact(v.clone()),
							[
								(
									_npos::codec::Compact(inner[0usize].0.clone()),
									_npos::codec::Compact(inner[0usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[1usize].0.clone()),
									_npos::codec::Compact(inner[1usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[2usize].0.clone()),
									_npos::codec::Compact(inner[2usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[3usize].0.clone()),
									_npos::codec::Compact(inner[3usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[4usize].0.clone()),
									_npos::codec::Compact(inner[4usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[5usize].0.clone()),
									_npos::codec::Compact(inner[5usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[6usize].0.clone()),
									_npos::codec::Compact(inner[6usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[7usize].0.clone()),
									_npos::codec::Compact(inner[7usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[8usize].0.clone()),
									_npos::codec::Compact(inner[8usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[9usize].0.clone()),
									_npos::codec::Compact(inner[9usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[10usize].0.clone()),
									_npos::codec::Compact(inner[10usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[11usize].0.clone()),
									_npos::codec::Compact(inner[11usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[12usize].0.clone()),
									_npos::codec::Compact(inner[12usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[13usize].0.clone()),
									_npos::codec::Compact(inner[13usize].1.clone()),
								),
								(
									_npos::codec::Compact(inner[14usize].0.clone()),
									_npos::codec::Compact(inner[14usize].1.clone()),
								),
							],
							_npos::codec::Compact(t_last.clone()),
						)
					})
					.collect::<Vec<_>>();
				votes16.encode_to(&mut r);
				r
			}
		}
		impl _npos::codec::Decode for TestCompact {
			fn decode<I: _npos::codec::Input>(value: &mut I) -> Result<Self, _npos::codec::Error> {
				let votes1 = < Vec < ( _npos :: codec :: Compact < u32 > , _npos :: codec :: Compact < u16 > ) > as _npos :: codec :: Decode > :: decode ( value ) ? ;
				let votes1 = votes1
					.into_iter()
					.map(|(v, t)| (v.0, t.0))
					.collect::<Vec<_>>();
				let votes2 = <Vec<(
					_npos::codec::Compact<u32>,
					(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>),
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes2 = votes2
					.into_iter()
					.map(|(v, (t1, w), t2)| (v.0, (t1.0, w.0), t2.0))
					.collect::<Vec<_>>();
				let votes3 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 3usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes3 = votes3
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes4 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 4usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes4 = votes4
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes5 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 5usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes5 = votes5
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes6 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 6usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes6 = votes6
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes7 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 7usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes7 = votes7
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
								((inner[5usize].0).0, (inner[5usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes8 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 8usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes8 = votes8
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
								((inner[5usize].0).0, (inner[5usize].1).0),
								((inner[6usize].0).0, (inner[6usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes9 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 9usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes9 = votes9
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
								((inner[5usize].0).0, (inner[5usize].1).0),
								((inner[6usize].0).0, (inner[6usize].1).0),
								((inner[7usize].0).0, (inner[7usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes10 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 10usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes10 = votes10
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
								((inner[5usize].0).0, (inner[5usize].1).0),
								((inner[6usize].0).0, (inner[6usize].1).0),
								((inner[7usize].0).0, (inner[7usize].1).0),
								((inner[8usize].0).0, (inner[8usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes11 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 11usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes11 = votes11
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
								((inner[5usize].0).0, (inner[5usize].1).0),
								((inner[6usize].0).0, (inner[6usize].1).0),
								((inner[7usize].0).0, (inner[7usize].1).0),
								((inner[8usize].0).0, (inner[8usize].1).0),
								((inner[9usize].0).0, (inner[9usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes12 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 12usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes12 = votes12
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
								((inner[5usize].0).0, (inner[5usize].1).0),
								((inner[6usize].0).0, (inner[6usize].1).0),
								((inner[7usize].0).0, (inner[7usize].1).0),
								((inner[8usize].0).0, (inner[8usize].1).0),
								((inner[9usize].0).0, (inner[9usize].1).0),
								((inner[10usize].0).0, (inner[10usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes13 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 13usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes13 = votes13
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
								((inner[5usize].0).0, (inner[5usize].1).0),
								((inner[6usize].0).0, (inner[6usize].1).0),
								((inner[7usize].0).0, (inner[7usize].1).0),
								((inner[8usize].0).0, (inner[8usize].1).0),
								((inner[9usize].0).0, (inner[9usize].1).0),
								((inner[10usize].0).0, (inner[10usize].1).0),
								((inner[11usize].0).0, (inner[11usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes14 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 14usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes14 = votes14
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
								((inner[5usize].0).0, (inner[5usize].1).0),
								((inner[6usize].0).0, (inner[6usize].1).0),
								((inner[7usize].0).0, (inner[7usize].1).0),
								((inner[8usize].0).0, (inner[8usize].1).0),
								((inner[9usize].0).0, (inner[9usize].1).0),
								((inner[10usize].0).0, (inner[10usize].1).0),
								((inner[11usize].0).0, (inner[11usize].1).0),
								((inner[12usize].0).0, (inner[12usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes15 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 15usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes15 = votes15
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
								((inner[5usize].0).0, (inner[5usize].1).0),
								((inner[6usize].0).0, (inner[6usize].1).0),
								((inner[7usize].0).0, (inner[7usize].1).0),
								((inner[8usize].0).0, (inner[8usize].1).0),
								((inner[9usize].0).0, (inner[9usize].1).0),
								((inner[10usize].0).0, (inner[10usize].1).0),
								((inner[11usize].0).0, (inner[11usize].1).0),
								((inner[12usize].0).0, (inner[12usize].1).0),
								((inner[13usize].0).0, (inner[13usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				let votes16 = <Vec<(
					_npos::codec::Compact<u32>,
					[(_npos::codec::Compact<u16>, _npos::codec::Compact<PerU16>); 16usize - 1],
					_npos::codec::Compact<u16>,
				)> as _npos::codec::Decode>::decode(value)?;
				let votes16 = votes16
					.into_iter()
					.map(|(v, inner, t_last)| {
						(
							v.0,
							[
								((inner[0usize].0).0, (inner[0usize].1).0),
								((inner[1usize].0).0, (inner[1usize].1).0),
								((inner[2usize].0).0, (inner[2usize].1).0),
								((inner[3usize].0).0, (inner[3usize].1).0),
								((inner[4usize].0).0, (inner[4usize].1).0),
								((inner[5usize].0).0, (inner[5usize].1).0),
								((inner[6usize].0).0, (inner[6usize].1).0),
								((inner[7usize].0).0, (inner[7usize].1).0),
								((inner[8usize].0).0, (inner[8usize].1).0),
								((inner[9usize].0).0, (inner[9usize].1).0),
								((inner[10usize].0).0, (inner[10usize].1).0),
								((inner[11usize].0).0, (inner[11usize].1).0),
								((inner[12usize].0).0, (inner[12usize].1).0),
								((inner[13usize].0).0, (inner[13usize].1).0),
								((inner[14usize].0).0, (inner[14usize].1).0),
							],
							t_last.0,
						)
					})
					.collect::<Vec<_>>();
				Ok(TestCompact {
					votes1,
					votes2,
					votes3,
					votes4,
					votes5,
					votes6,
					votes7,
					votes8,
					votes9,
					votes10,
					votes11,
					votes12,
					votes13,
					votes14,
					votes15,
					votes16,
				})
			}
		}
		pub struct TestCompact {
			votes1: Vec<(u32, u16)>,
			votes2: Vec<(u32, (u16, PerU16), u16)>,
			votes3: Vec<(u32, [(u16, PerU16); 2usize], u16)>,
			votes4: Vec<(u32, [(u16, PerU16); 3usize], u16)>,
			votes5: Vec<(u32, [(u16, PerU16); 4usize], u16)>,
			votes6: Vec<(u32, [(u16, PerU16); 5usize], u16)>,
			votes7: Vec<(u32, [(u16, PerU16); 6usize], u16)>,
			votes8: Vec<(u32, [(u16, PerU16); 7usize], u16)>,
			votes9: Vec<(u32, [(u16, PerU16); 8usize], u16)>,
			votes10: Vec<(u32, [(u16, PerU16); 9usize], u16)>,
			votes11: Vec<(u32, [(u16, PerU16); 10usize], u16)>,
			votes12: Vec<(u32, [(u16, PerU16); 11usize], u16)>,
			votes13: Vec<(u32, [(u16, PerU16); 12usize], u16)>,
			votes14: Vec<(u32, [(u16, PerU16); 13usize], u16)>,
			votes15: Vec<(u32, [(u16, PerU16); 14usize], u16)>,
			votes16: Vec<(u32, [(u16, PerU16); 15usize], u16)>,
		}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::default::Default for TestCompact {
			#[inline]
			fn default() -> TestCompact {
				TestCompact {
					votes1: ::core::default::Default::default(),
					votes2: ::core::default::Default::default(),
					votes3: ::core::default::Default::default(),
					votes4: ::core::default::Default::default(),
					votes5: ::core::default::Default::default(),
					votes6: ::core::default::Default::default(),
					votes7: ::core::default::Default::default(),
					votes8: ::core::default::Default::default(),
					votes9: ::core::default::Default::default(),
					votes10: ::core::default::Default::default(),
					votes11: ::core::default::Default::default(),
					votes12: ::core::default::Default::default(),
					votes13: ::core::default::Default::default(),
					votes14: ::core::default::Default::default(),
					votes15: ::core::default::Default::default(),
					votes16: ::core::default::Default::default(),
				}
			}
		}
		impl ::core::marker::StructuralPartialEq for TestCompact {}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::cmp::PartialEq for TestCompact {
			#[inline]
			fn eq(&self, other: &TestCompact) -> bool {
				match *other {
					TestCompact {
						votes1: ref __self_1_0,
						votes2: ref __self_1_1,
						votes3: ref __self_1_2,
						votes4: ref __self_1_3,
						votes5: ref __self_1_4,
						votes6: ref __self_1_5,
						votes7: ref __self_1_6,
						votes8: ref __self_1_7,
						votes9: ref __self_1_8,
						votes10: ref __self_1_9,
						votes11: ref __self_1_10,
						votes12: ref __self_1_11,
						votes13: ref __self_1_12,
						votes14: ref __self_1_13,
						votes15: ref __self_1_14,
						votes16: ref __self_1_15,
					} => {
						match *self {
							TestCompact {
								votes1: ref __self_0_0,
								votes2: ref __self_0_1,
								votes3: ref __self_0_2,
								votes4: ref __self_0_3,
								votes5: ref __self_0_4,
								votes6: ref __self_0_5,
								votes7: ref __self_0_6,
								votes8: ref __self_0_7,
								votes9: ref __self_0_8,
								votes10: ref __self_0_9,
								votes11: ref __self_0_10,
								votes12: ref __self_0_11,
								votes13: ref __self_0_12,
								votes14: ref __self_0_13,
								votes15: ref __self_0_14,
								votes16: ref __self_0_15,
							} => {
								(*__self_0_0) == (*__self_1_0)
									&& (*__self_0_1) == (*__self_1_1) && (*__self_0_2) == (*__self_1_2)
									&& (*__self_0_3) == (*__self_1_3) && (*__self_0_4) == (*__self_1_4)
									&& (*__self_0_5) == (*__self_1_5) && (*__self_0_6) == (*__self_1_6)
									&& (*__self_0_7) == (*__self_1_7) && (*__self_0_8) == (*__self_1_8)
									&& (*__self_0_9) == (*__self_1_9) && (*__self_0_10)
									== (*__self_1_10) && (*__self_0_11) == (*__self_1_11)
									&& (*__self_0_12) == (*__self_1_12) && (*__self_0_13)
									== (*__self_1_13) && (*__self_0_14) == (*__self_1_14)
									&& (*__self_0_15) == (*__self_1_15)
							}
						}
					}
				}
			}
			#[inline]
			fn ne(&self, other: &TestCompact) -> bool {
				match *other {
					TestCompact {
						votes1: ref __self_1_0,
						votes2: ref __self_1_1,
						votes3: ref __self_1_2,
						votes4: ref __self_1_3,
						votes5: ref __self_1_4,
						votes6: ref __self_1_5,
						votes7: ref __self_1_6,
						votes8: ref __self_1_7,
						votes9: ref __self_1_8,
						votes10: ref __self_1_9,
						votes11: ref __self_1_10,
						votes12: ref __self_1_11,
						votes13: ref __self_1_12,
						votes14: ref __self_1_13,
						votes15: ref __self_1_14,
						votes16: ref __self_1_15,
					} => {
						match *self {
							TestCompact {
								votes1: ref __self_0_0,
								votes2: ref __self_0_1,
								votes3: ref __self_0_2,
								votes4: ref __self_0_3,
								votes5: ref __self_0_4,
								votes6: ref __self_0_5,
								votes7: ref __self_0_6,
								votes8: ref __self_0_7,
								votes9: ref __self_0_8,
								votes10: ref __self_0_9,
								votes11: ref __self_0_10,
								votes12: ref __self_0_11,
								votes13: ref __self_0_12,
								votes14: ref __self_0_13,
								votes15: ref __self_0_14,
								votes16: ref __self_0_15,
							} => {
								(*__self_0_0) != (*__self_1_0)
									|| (*__self_0_1) != (*__self_1_1) || (*__self_0_2) != (*__self_1_2)
									|| (*__self_0_3) != (*__self_1_3) || (*__self_0_4) != (*__self_1_4)
									|| (*__self_0_5) != (*__self_1_5) || (*__self_0_6) != (*__self_1_6)
									|| (*__self_0_7) != (*__self_1_7) || (*__self_0_8) != (*__self_1_8)
									|| (*__self_0_9) != (*__self_1_9) || (*__self_0_10)
									!= (*__self_1_10) || (*__self_0_11) != (*__self_1_11)
									|| (*__self_0_12) != (*__self_1_12) || (*__self_0_13)
									!= (*__self_1_13) || (*__self_0_14) != (*__self_1_14)
									|| (*__self_0_15) != (*__self_1_15)
							}
						}
					}
				}
			}
		}
		impl ::core::marker::StructuralEq for TestCompact {}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::cmp::Eq for TestCompact {
			#[inline]
			#[doc(hidden)]
			fn assert_receiver_is_total_eq(&self) -> () {
				{
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, (u16, PerU16), u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 2usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 3usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 4usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 5usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 6usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 7usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 8usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 9usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 10usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 11usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 12usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 13usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 14usize], u16)>>;
					let _: ::core::cmp::AssertParamIsEq<Vec<(u32, [(u16, PerU16); 15usize], u16)>>;
				}
			}
		}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::clone::Clone for TestCompact {
			#[inline]
			fn clone(&self) -> TestCompact {
				match *self {
					TestCompact {
						votes1: ref __self_0_0,
						votes2: ref __self_0_1,
						votes3: ref __self_0_2,
						votes4: ref __self_0_3,
						votes5: ref __self_0_4,
						votes6: ref __self_0_5,
						votes7: ref __self_0_6,
						votes8: ref __self_0_7,
						votes9: ref __self_0_8,
						votes10: ref __self_0_9,
						votes11: ref __self_0_10,
						votes12: ref __self_0_11,
						votes13: ref __self_0_12,
						votes14: ref __self_0_13,
						votes15: ref __self_0_14,
						votes16: ref __self_0_15,
					} => TestCompact {
						votes1: ::core::clone::Clone::clone(&(*__self_0_0)),
						votes2: ::core::clone::Clone::clone(&(*__self_0_1)),
						votes3: ::core::clone::Clone::clone(&(*__self_0_2)),
						votes4: ::core::clone::Clone::clone(&(*__self_0_3)),
						votes5: ::core::clone::Clone::clone(&(*__self_0_4)),
						votes6: ::core::clone::Clone::clone(&(*__self_0_5)),
						votes7: ::core::clone::Clone::clone(&(*__self_0_6)),
						votes8: ::core::clone::Clone::clone(&(*__self_0_7)),
						votes9: ::core::clone::Clone::clone(&(*__self_0_8)),
						votes10: ::core::clone::Clone::clone(&(*__self_0_9)),
						votes11: ::core::clone::Clone::clone(&(*__self_0_10)),
						votes12: ::core::clone::Clone::clone(&(*__self_0_11)),
						votes13: ::core::clone::Clone::clone(&(*__self_0_12)),
						votes14: ::core::clone::Clone::clone(&(*__self_0_13)),
						votes15: ::core::clone::Clone::clone(&(*__self_0_14)),
						votes16: ::core::clone::Clone::clone(&(*__self_0_15)),
					},
				}
			}
		}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::fmt::Debug for TestCompact {
			fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
				match *self {
					TestCompact {
						votes1: ref __self_0_0,
						votes2: ref __self_0_1,
						votes3: ref __self_0_2,
						votes4: ref __self_0_3,
						votes5: ref __self_0_4,
						votes6: ref __self_0_5,
						votes7: ref __self_0_6,
						votes8: ref __self_0_7,
						votes9: ref __self_0_8,
						votes10: ref __self_0_9,
						votes11: ref __self_0_10,
						votes12: ref __self_0_11,
						votes13: ref __self_0_12,
						votes14: ref __self_0_13,
						votes15: ref __self_0_14,
						votes16: ref __self_0_15,
					} => {
						let mut debug_trait_builder = f.debug_struct("TestCompact");
						let _ = debug_trait_builder.field("votes1", &&(*__self_0_0));
						let _ = debug_trait_builder.field("votes2", &&(*__self_0_1));
						let _ = debug_trait_builder.field("votes3", &&(*__self_0_2));
						let _ = debug_trait_builder.field("votes4", &&(*__self_0_3));
						let _ = debug_trait_builder.field("votes5", &&(*__self_0_4));
						let _ = debug_trait_builder.field("votes6", &&(*__self_0_5));
						let _ = debug_trait_builder.field("votes7", &&(*__self_0_6));
						let _ = debug_trait_builder.field("votes8", &&(*__self_0_7));
						let _ = debug_trait_builder.field("votes9", &&(*__self_0_8));
						let _ = debug_trait_builder.field("votes10", &&(*__self_0_9));
						let _ = debug_trait_builder.field("votes11", &&(*__self_0_10));
						let _ = debug_trait_builder.field("votes12", &&(*__self_0_11));
						let _ = debug_trait_builder.field("votes13", &&(*__self_0_12));
						let _ = debug_trait_builder.field("votes14", &&(*__self_0_13));
						let _ = debug_trait_builder.field("votes15", &&(*__self_0_14));
						let _ = debug_trait_builder.field("votes16", &&(*__self_0_15));
						debug_trait_builder.finish()
					}
				}
			}
		}
		use _npos::__OrInvalidIndex;
		impl _npos::CompactSolution for TestCompact {
			const LIMIT: usize = 16usize;
			type Voter = u32;
			type Target = u16;
			type VoteWeight = PerU16;
			fn len(&self) -> usize {
				let mut all_len = 0usize;
				all_len = all_len.saturating_add(self.votes1.len());
				all_len = all_len.saturating_add(self.votes2.len());
				all_len = all_len.saturating_add(self.votes3.len());
				all_len = all_len.saturating_add(self.votes4.len());
				all_len = all_len.saturating_add(self.votes5.len());
				all_len = all_len.saturating_add(self.votes6.len());
				all_len = all_len.saturating_add(self.votes7.len());
				all_len = all_len.saturating_add(self.votes8.len());
				all_len = all_len.saturating_add(self.votes9.len());
				all_len = all_len.saturating_add(self.votes10.len());
				all_len = all_len.saturating_add(self.votes11.len());
				all_len = all_len.saturating_add(self.votes12.len());
				all_len = all_len.saturating_add(self.votes13.len());
				all_len = all_len.saturating_add(self.votes14.len());
				all_len = all_len.saturating_add(self.votes15.len());
				all_len = all_len.saturating_add(self.votes16.len());
				all_len
			}
			fn edge_count(&self) -> usize {
				let mut all_edges = 0usize;
				all_edges =
					all_edges.saturating_add(self.votes1.len().saturating_mul(1usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes2.len().saturating_mul(2usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes3.len().saturating_mul(3usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes4.len().saturating_mul(4usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes5.len().saturating_mul(5usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes6.len().saturating_mul(6usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes7.len().saturating_mul(7usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes8.len().saturating_mul(8usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes9.len().saturating_mul(9usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes10.len().saturating_mul(10usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes11.len().saturating_mul(11usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes12.len().saturating_mul(12usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes13.len().saturating_mul(13usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes14.len().saturating_mul(14usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes15.len().saturating_mul(15usize as usize));
				all_edges =
					all_edges.saturating_add(self.votes16.len().saturating_mul(16usize as usize));
				all_edges
			}
			fn unique_targets(&self) -> Vec<Self::Target> {
				let mut all_targets: Vec<Self::Target> =
					Vec::with_capacity(self.average_edge_count());
				let mut maybe_insert_target = |t: Self::Target| match all_targets.binary_search(&t)
				{
					Ok(_) => (),
					Err(pos) => all_targets.insert(pos, t),
				};
				self.votes1.iter().for_each(|(_, t)| {
					maybe_insert_target(*t);
				});
				self.votes2.iter().for_each(|(_, (t1, _), t2)| {
					maybe_insert_target(*t1);
					maybe_insert_target(*t2);
				});
				self.votes3.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes4.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes5.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes6.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes7.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes8.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes9.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes10.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes11.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes12.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes13.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes14.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes15.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				self.votes16.iter().for_each(|(_, inners, t_last)| {
					inners.iter().for_each(|(t, _)| {
						maybe_insert_target(*t);
					});
					maybe_insert_target(*t_last);
				});
				all_targets
			}
			fn remove_voter(&mut self, to_remove: Self::Voter) -> bool {
				if let Some(idx) = self.votes1.iter().position(|(x, _)| *x == to_remove) {
					self.votes1.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes2.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes2.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes3.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes3.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes4.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes4.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes5.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes5.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes6.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes6.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes7.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes7.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes8.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes8.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes9.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes9.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes10.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes10.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes11.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes11.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes12.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes12.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes13.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes13.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes14.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes14.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes15.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes15.remove(idx);
					return true;
				}
				if let Some(idx) = self.votes16.iter().position(|(x, _, _)| *x == to_remove) {
					self.votes16.remove(idx);
					return true;
				}
				return false;
			}
			fn from_assignment<FV, FT, A>(
				assignments: Vec<_npos::Assignment<A, PerU16>>,
				index_of_voter: FV,
				index_of_target: FT,
			) -> Result<Self, _npos::Error>
			where
				A: _npos::IdentifierT,
				for<'r> FV: Fn(&'r A) -> Option<Self::Voter>,
				for<'r> FT: Fn(&'r A) -> Option<Self::Target>,
			{
				let mut compact: TestCompact = Default::default();
				for _npos::Assignment { who, distribution } in assignments {
					match distribution.len() {
						0 => continue,
						1 => compact.votes1.push((
							index_of_voter(&who).or_invalid_index()?,
							index_of_target(&distribution[0].0).or_invalid_index()?,
						)),
						2 => compact.votes2.push((
							index_of_voter(&who).or_invalid_index()?,
							(
								index_of_target(&distribution[0].0).or_invalid_index()?,
								distribution[0].1,
							),
							index_of_target(&distribution[1].0).or_invalid_index()?,
						)),
						3usize => compact.votes3.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
							],
							index_of_target(&distribution[2usize].0).or_invalid_index()?,
						)),
						4usize => compact.votes4.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
							],
							index_of_target(&distribution[3usize].0).or_invalid_index()?,
						)),
						5usize => compact.votes5.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
							],
							index_of_target(&distribution[4usize].0).or_invalid_index()?,
						)),
						6usize => compact.votes6.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
							],
							index_of_target(&distribution[5usize].0).or_invalid_index()?,
						)),
						7usize => compact.votes7.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
								(
									index_of_target(&distribution[5usize].0).or_invalid_index()?,
									distribution[5usize].1,
								),
							],
							index_of_target(&distribution[6usize].0).or_invalid_index()?,
						)),
						8usize => compact.votes8.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
								(
									index_of_target(&distribution[5usize].0).or_invalid_index()?,
									distribution[5usize].1,
								),
								(
									index_of_target(&distribution[6usize].0).or_invalid_index()?,
									distribution[6usize].1,
								),
							],
							index_of_target(&distribution[7usize].0).or_invalid_index()?,
						)),
						9usize => compact.votes9.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
								(
									index_of_target(&distribution[5usize].0).or_invalid_index()?,
									distribution[5usize].1,
								),
								(
									index_of_target(&distribution[6usize].0).or_invalid_index()?,
									distribution[6usize].1,
								),
								(
									index_of_target(&distribution[7usize].0).or_invalid_index()?,
									distribution[7usize].1,
								),
							],
							index_of_target(&distribution[8usize].0).or_invalid_index()?,
						)),
						10usize => compact.votes10.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
								(
									index_of_target(&distribution[5usize].0).or_invalid_index()?,
									distribution[5usize].1,
								),
								(
									index_of_target(&distribution[6usize].0).or_invalid_index()?,
									distribution[6usize].1,
								),
								(
									index_of_target(&distribution[7usize].0).or_invalid_index()?,
									distribution[7usize].1,
								),
								(
									index_of_target(&distribution[8usize].0).or_invalid_index()?,
									distribution[8usize].1,
								),
							],
							index_of_target(&distribution[9usize].0).or_invalid_index()?,
						)),
						11usize => compact.votes11.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
								(
									index_of_target(&distribution[5usize].0).or_invalid_index()?,
									distribution[5usize].1,
								),
								(
									index_of_target(&distribution[6usize].0).or_invalid_index()?,
									distribution[6usize].1,
								),
								(
									index_of_target(&distribution[7usize].0).or_invalid_index()?,
									distribution[7usize].1,
								),
								(
									index_of_target(&distribution[8usize].0).or_invalid_index()?,
									distribution[8usize].1,
								),
								(
									index_of_target(&distribution[9usize].0).or_invalid_index()?,
									distribution[9usize].1,
								),
							],
							index_of_target(&distribution[10usize].0).or_invalid_index()?,
						)),
						12usize => compact.votes12.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
								(
									index_of_target(&distribution[5usize].0).or_invalid_index()?,
									distribution[5usize].1,
								),
								(
									index_of_target(&distribution[6usize].0).or_invalid_index()?,
									distribution[6usize].1,
								),
								(
									index_of_target(&distribution[7usize].0).or_invalid_index()?,
									distribution[7usize].1,
								),
								(
									index_of_target(&distribution[8usize].0).or_invalid_index()?,
									distribution[8usize].1,
								),
								(
									index_of_target(&distribution[9usize].0).or_invalid_index()?,
									distribution[9usize].1,
								),
								(
									index_of_target(&distribution[10usize].0).or_invalid_index()?,
									distribution[10usize].1,
								),
							],
							index_of_target(&distribution[11usize].0).or_invalid_index()?,
						)),
						13usize => compact.votes13.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
								(
									index_of_target(&distribution[5usize].0).or_invalid_index()?,
									distribution[5usize].1,
								),
								(
									index_of_target(&distribution[6usize].0).or_invalid_index()?,
									distribution[6usize].1,
								),
								(
									index_of_target(&distribution[7usize].0).or_invalid_index()?,
									distribution[7usize].1,
								),
								(
									index_of_target(&distribution[8usize].0).or_invalid_index()?,
									distribution[8usize].1,
								),
								(
									index_of_target(&distribution[9usize].0).or_invalid_index()?,
									distribution[9usize].1,
								),
								(
									index_of_target(&distribution[10usize].0).or_invalid_index()?,
									distribution[10usize].1,
								),
								(
									index_of_target(&distribution[11usize].0).or_invalid_index()?,
									distribution[11usize].1,
								),
							],
							index_of_target(&distribution[12usize].0).or_invalid_index()?,
						)),
						14usize => compact.votes14.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
								(
									index_of_target(&distribution[5usize].0).or_invalid_index()?,
									distribution[5usize].1,
								),
								(
									index_of_target(&distribution[6usize].0).or_invalid_index()?,
									distribution[6usize].1,
								),
								(
									index_of_target(&distribution[7usize].0).or_invalid_index()?,
									distribution[7usize].1,
								),
								(
									index_of_target(&distribution[8usize].0).or_invalid_index()?,
									distribution[8usize].1,
								),
								(
									index_of_target(&distribution[9usize].0).or_invalid_index()?,
									distribution[9usize].1,
								),
								(
									index_of_target(&distribution[10usize].0).or_invalid_index()?,
									distribution[10usize].1,
								),
								(
									index_of_target(&distribution[11usize].0).or_invalid_index()?,
									distribution[11usize].1,
								),
								(
									index_of_target(&distribution[12usize].0).or_invalid_index()?,
									distribution[12usize].1,
								),
							],
							index_of_target(&distribution[13usize].0).or_invalid_index()?,
						)),
						15usize => compact.votes15.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
								(
									index_of_target(&distribution[5usize].0).or_invalid_index()?,
									distribution[5usize].1,
								),
								(
									index_of_target(&distribution[6usize].0).or_invalid_index()?,
									distribution[6usize].1,
								),
								(
									index_of_target(&distribution[7usize].0).or_invalid_index()?,
									distribution[7usize].1,
								),
								(
									index_of_target(&distribution[8usize].0).or_invalid_index()?,
									distribution[8usize].1,
								),
								(
									index_of_target(&distribution[9usize].0).or_invalid_index()?,
									distribution[9usize].1,
								),
								(
									index_of_target(&distribution[10usize].0).or_invalid_index()?,
									distribution[10usize].1,
								),
								(
									index_of_target(&distribution[11usize].0).or_invalid_index()?,
									distribution[11usize].1,
								),
								(
									index_of_target(&distribution[12usize].0).or_invalid_index()?,
									distribution[12usize].1,
								),
								(
									index_of_target(&distribution[13usize].0).or_invalid_index()?,
									distribution[13usize].1,
								),
							],
							index_of_target(&distribution[14usize].0).or_invalid_index()?,
						)),
						16usize => compact.votes16.push((
							index_of_voter(&who).or_invalid_index()?,
							[
								(
									index_of_target(&distribution[0usize].0).or_invalid_index()?,
									distribution[0usize].1,
								),
								(
									index_of_target(&distribution[1usize].0).or_invalid_index()?,
									distribution[1usize].1,
								),
								(
									index_of_target(&distribution[2usize].0).or_invalid_index()?,
									distribution[2usize].1,
								),
								(
									index_of_target(&distribution[3usize].0).or_invalid_index()?,
									distribution[3usize].1,
								),
								(
									index_of_target(&distribution[4usize].0).or_invalid_index()?,
									distribution[4usize].1,
								),
								(
									index_of_target(&distribution[5usize].0).or_invalid_index()?,
									distribution[5usize].1,
								),
								(
									index_of_target(&distribution[6usize].0).or_invalid_index()?,
									distribution[6usize].1,
								),
								(
									index_of_target(&distribution[7usize].0).or_invalid_index()?,
									distribution[7usize].1,
								),
								(
									index_of_target(&distribution[8usize].0).or_invalid_index()?,
									distribution[8usize].1,
								),
								(
									index_of_target(&distribution[9usize].0).or_invalid_index()?,
									distribution[9usize].1,
								),
								(
									index_of_target(&distribution[10usize].0).or_invalid_index()?,
									distribution[10usize].1,
								),
								(
									index_of_target(&distribution[11usize].0).or_invalid_index()?,
									distribution[11usize].1,
								),
								(
									index_of_target(&distribution[12usize].0).or_invalid_index()?,
									distribution[12usize].1,
								),
								(
									index_of_target(&distribution[13usize].0).or_invalid_index()?,
									distribution[13usize].1,
								),
								(
									index_of_target(&distribution[14usize].0).or_invalid_index()?,
									distribution[14usize].1,
								),
							],
							index_of_target(&distribution[15usize].0).or_invalid_index()?,
						)),
						_ => {
							return Err(_npos::Error::CompactTargetOverflow);
						}
					}
				}
				Ok(compact)
			}
			fn into_assignment<A: _npos::IdentifierT>(
				self,
				voter_at: impl Fn(Self::Voter) -> Option<A>,
				target_at: impl Fn(Self::Target) -> Option<A>,
			) -> Result<Vec<_npos::Assignment<A, PerU16>>, _npos::Error> {
				let mut assignments: Vec<_npos::Assignment<A, PerU16>> = Default::default();
				for (voter_index, target_index) in self.votes1 {
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: <[_]>::into_vec(box [(
							target_at(target_index).or_invalid_index()?,
							PerU16::one(),
						)]),
					})
				}
				for (voter_index, (t1_idx, p1), t2_idx) in self.votes2 {
					if p1 >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p2 =
						_npos::sp_arithmetic::traits::Saturating::saturating_sub(PerU16::one(), p1);
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: <[_]>::into_vec(box [
							(target_at(t1_idx).or_invalid_index()?, p1),
							(target_at(t2_idx).or_invalid_index()?, p2),
						]),
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes3 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes4 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes5 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes6 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes7 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes8 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes9 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes10 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes11 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes12 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes13 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes14 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes15 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				for (voter_index, inners, t_last_idx) in self.votes16 {
					let mut sum = PerU16::zero();
					let mut inners_parsed = inners
						.iter()
						.map(|(ref t_idx, p)| {
							sum = _npos::sp_arithmetic::traits::Saturating::saturating_add(sum, *p);
							let target = target_at(*t_idx).or_invalid_index()?;
							Ok((target, *p))
						})
						.collect::<Result<Vec<(A, PerU16)>, _npos::Error>>()?;
					if sum >= PerU16::one() {
						return Err(_npos::Error::CompactStakeOverflow);
					}
					let p_last = _npos::sp_arithmetic::traits::Saturating::saturating_sub(
						PerU16::one(),
						sum,
					);
					inners_parsed.push((target_at(t_last_idx).or_invalid_index()?, p_last));
					assignments.push(_npos::Assignment {
						who: voter_at(voter_index).or_invalid_index()?,
						distribution: inners_parsed,
					});
				}
				Ok(assignments)
			}
		}
		/// To from `now` to block `n`.
		pub fn roll_to(n: u64) {
			let now = System::block_number();
			for i in now + 1..=n {
				System::set_block_number(i);
				TwoPhase::on_initialize(i);
			}
		}
		/// Get the free and reserved balance of some account.
		pub fn balances(who: &AccountId) -> (Balance, Balance) {
			(Balances::free_balance(who), Balances::reserved_balance(who))
		}
		/// Spit out a verifiable raw solution.
		///
		/// This is a good example of what an offchain miner would do.
		pub fn raw_solution() -> RawSolution<CompactOf<Runtime>> {
			let voters = TwoPhase::snapshot_voters().unwrap();
			let targets = TwoPhase::snapshot_targets().unwrap();
			let desired = TwoPhase::desired_targets() as usize;
			let voter_index =
				|who: &AccountId| -> Option<crate::two_phase::CompactVoterIndexOf<Runtime>> {
					voters . iter ( ) . position ( | ( x , _ , _ ) | x == who ) . and_then ( | i | < usize as crate :: TryInto < crate :: two_phase :: CompactVoterIndexOf < Runtime > > > :: try_into ( i ) . ok ( ) )
				};
			let target_index =
				|who: &AccountId| -> Option<crate::two_phase::CompactTargetIndexOf<Runtime>> {
					targets . iter ( ) . position ( | x | x == who ) . and_then ( | i | < usize as crate :: TryInto < crate :: two_phase :: CompactTargetIndexOf < Runtime > > > :: try_into ( i ) . ok ( ) )
				};
			let stake_of = |who: &AccountId| -> crate::VoteWeight {
				voters
					.iter()
					.find(|(x, _, _)| x == who)
					.map(|(_, x, _)| *x)
					.unwrap_or_default()
			};
			let ElectionResult {
				winners,
				assignments,
			} = seq_phragmen::<_, CompactAccuracyOf<Runtime>>(
				desired,
				targets.clone(),
				voters.clone(),
				None,
			)
			.unwrap();
			let winners = to_without_backing(winners);
			let score = {
				let staked =
					assignment_ratio_to_staked_normalized(assignments.clone(), &stake_of).unwrap();
				to_supports(&winners, &staked).unwrap().evaluate()
			};
			let compact =
				<CompactOf<Runtime>>::from_assignment(assignments, &voter_index, &target_index)
					.unwrap();
			RawSolution { compact, score }
		}
		/// Creates a **valid** solution with exactly the given size.
		///
		/// The snapshot size must be bigger, otherwise this will panic.
		pub fn solution_with_size(
			active_voters: u32,
			winners_count: u32,
		) -> RawSolution<CompactOf<Runtime>> {
			use rand::seq::SliceRandom;
			let voters = TwoPhase::snapshot_voters().unwrap();
			let targets = TwoPhase::snapshot_targets().unwrap();
			if !(active_voters >= winners_count) {
				{
					::std::rt::begin_panic("assertion failed: active_voters >= winners_count")
				}
			};
			if !(voters.len() >= active_voters as usize) {
				{
					::std::rt::begin_panic(
						"assertion failed: voters.len() >= active_voters as usize",
					)
				}
			};
			if !(targets.len() >= winners_count as usize) {
				{
					::std::rt::begin_panic(
						"assertion failed: targets.len() >= winners_count as usize",
					)
				}
			};
			let voter_index =
				|who: &AccountId| -> Option<crate::two_phase::CompactVoterIndexOf<Runtime>> {
					voters . iter ( ) . position ( | ( x , _ , _ ) | x == who ) . and_then ( | i | < usize as crate :: TryInto < crate :: two_phase :: CompactVoterIndexOf < Runtime > > > :: try_into ( i ) . ok ( ) )
				};
			let voter_at =
				|i: crate::two_phase::CompactVoterIndexOf<Runtime>| -> Option<AccountId> {
					< crate :: two_phase :: CompactVoterIndexOf < Runtime > as crate :: TryInto < usize > > :: try_into ( i ) . ok ( ) . and_then ( | i | voters . get ( i ) . map ( | ( x , _ , _ ) | x ) . cloned ( ) )
				};
			let target_at =
				|i: crate::two_phase::CompactTargetIndexOf<Runtime>| -> Option<AccountId> {
					< crate :: two_phase :: CompactTargetIndexOf < Runtime > as crate :: TryInto < usize > > :: try_into ( i ) . ok ( ) . and_then ( | i | targets . get ( i ) . cloned ( ) )
				};
			let stake_of = |who: &AccountId| -> crate::VoteWeight {
				voters
					.iter()
					.find(|(x, _, _)| x == who)
					.map(|(_, x, _)| *x)
					.unwrap_or_default()
			};
			let mut rng = rand::thread_rng();
			let winners = targets
				.as_slice()
				.choose_multiple(&mut rng, winners_count as usize)
				.cloned()
				.collect::<Vec<_>>();
			let mut assignments = winners
				.iter()
				.map(|w| sp_npos_elections::Assignment {
					who: w,
					distribution: <[_]>::into_vec(box [(w, PerU16::one())]),
				})
				.collect::<Vec<_>>();
			let mut voters_pool = voters
				.iter()
				.filter(|(x, _, z)| *x != z[0])
				.cloned()
				.collect::<Vec<_>>();
			while assignments.len() < active_voters as usize {
				let voter = voters_pool.remove(rand::random::<usize>() % voters_pool.len());
			}
			{
				::std::rt::begin_panic("not implemented")
			}
		}
		pub enum OuterCall {
			TwoPhase(::frame_support::dispatch::CallableCallFor<TwoPhase, Runtime>),
		}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::clone::Clone for OuterCall {
			#[inline]
			fn clone(&self) -> OuterCall {
				match (&*self,) {
					(&OuterCall::TwoPhase(ref __self_0),) => {
						OuterCall::TwoPhase(::core::clone::Clone::clone(&(*__self_0)))
					}
				}
			}
		}
		impl ::core::marker::StructuralPartialEq for OuterCall {}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::cmp::PartialEq for OuterCall {
			#[inline]
			fn eq(&self, other: &OuterCall) -> bool {
				match (&*self, &*other) {
					(&OuterCall::TwoPhase(ref __self_0), &OuterCall::TwoPhase(ref __arg_1_0)) => {
						(*__self_0) == (*__arg_1_0)
					}
				}
			}
			#[inline]
			fn ne(&self, other: &OuterCall) -> bool {
				match (&*self, &*other) {
					(&OuterCall::TwoPhase(ref __self_0), &OuterCall::TwoPhase(ref __arg_1_0)) => {
						(*__self_0) != (*__arg_1_0)
					}
				}
			}
		}
		impl ::core::marker::StructuralEq for OuterCall {}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::cmp::Eq for OuterCall {
			#[inline]
			#[doc(hidden)]
			fn assert_receiver_is_total_eq(&self) -> () {
				{
					let _: ::core::cmp::AssertParamIsEq<
						::frame_support::dispatch::CallableCallFor<TwoPhase, Runtime>,
					>;
				}
			}
		}
		const _: () = {
			#[allow(unknown_lints)]
			#[allow(rust_2018_idioms)]
			extern crate codec as _parity_scale_codec;
			impl _parity_scale_codec::Encode for OuterCall {
				fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {
					match *self {
						OuterCall::TwoPhase(ref aa) => {
							dest.push_byte(0usize as u8);
							dest.push(aa);
						}
						_ => (),
					}
				}
			}
			impl _parity_scale_codec::EncodeLike for OuterCall {}
		};
		const _: () = {
			#[allow(unknown_lints)]
			#[allow(rust_2018_idioms)]
			extern crate codec as _parity_scale_codec;
			impl _parity_scale_codec::Decode for OuterCall {
				fn decode<DecIn: _parity_scale_codec::Input>(
					input: &mut DecIn,
				) -> core::result::Result<Self, _parity_scale_codec::Error> {
					match input.read_byte()? {
						x if x == 0usize as u8 => Ok(OuterCall::TwoPhase({
							let res = _parity_scale_codec::Decode::decode(input);
							match res {
								Err(_) => {
									return Err(
										"Error decoding field OuterCall :: TwoPhase.0".into()
									)
								}
								Ok(a) => a,
							}
						})),
						x => Err("No such variant in enum OuterCall".into()),
					}
				}
			}
		};
		impl core::fmt::Debug for OuterCall {
			fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
				match self {
					Self::TwoPhase(ref a0) => {
						fmt.debug_tuple("OuterCall::TwoPhase").field(a0).finish()
					}
					_ => Ok(()),
				}
			}
		}
		impl ::frame_support::dispatch::GetDispatchInfo for OuterCall {
			fn get_dispatch_info(&self) -> ::frame_support::dispatch::DispatchInfo {
				match self {
					OuterCall::TwoPhase(call) => call.get_dispatch_info(),
				}
			}
		}
		impl ::frame_support::dispatch::GetCallMetadata for OuterCall {
			fn get_call_metadata(&self) -> ::frame_support::dispatch::CallMetadata {
				use ::frame_support::dispatch::GetCallName;
				match self {
					OuterCall::TwoPhase(call) => {
						let function_name = call.get_call_name();
						let pallet_name = "TwoPhase";
						::frame_support::dispatch::CallMetadata {
							function_name,
							pallet_name,
						}
					}
				}
			}
			fn get_module_names() -> &'static [&'static str] {
				&["TwoPhase"]
			}
			fn get_call_names(module: &str) -> &'static [&'static str] {
				use ::frame_support::dispatch::{Callable, GetCallName};
				match module {
					"TwoPhase" => {
						<<TwoPhase as Callable<Runtime>>::Call as GetCallName>::get_call_names()
					}
					_ => ::std::rt::begin_panic("internal error: entered unreachable code"),
				}
			}
		}
		impl ::frame_support::dispatch::Dispatchable for OuterCall {
			type Origin = Origin;
			type Trait = OuterCall;
			type Info = ::frame_support::weights::DispatchInfo;
			type PostInfo = ::frame_support::weights::PostDispatchInfo;
			fn dispatch(
				self,
				origin: Origin,
			) -> ::frame_support::dispatch::DispatchResultWithPostInfo {
				if !<Self::Origin as ::frame_support::traits::OriginTrait>::filter_call(
					&origin, &self,
				) {
					return ::frame_support::sp_std::result::Result::Err(
						::frame_support::dispatch::DispatchError::BadOrigin.into(),
					);
				}
				::frame_support::traits::UnfilteredDispatchable::dispatch_bypass_filter(
					self, origin,
				)
			}
		}
		impl ::frame_support::traits::UnfilteredDispatchable for OuterCall {
			type Origin = Origin;
			fn dispatch_bypass_filter(
				self,
				origin: Origin,
			) -> ::frame_support::dispatch::DispatchResultWithPostInfo {
				match self {
					OuterCall::TwoPhase(call) => {
						::frame_support::traits::UnfilteredDispatchable::dispatch_bypass_filter(
							call, origin,
						)
					}
				}
			}
		}
		impl
			::frame_support::dispatch::IsSubType<
				::frame_support::dispatch::CallableCallFor<TwoPhase, Runtime>,
			> for OuterCall
		{
			#[allow(unreachable_patterns)]
			fn is_sub_type(
				&self,
			) -> Option<&::frame_support::dispatch::CallableCallFor<TwoPhase, Runtime>> {
				match *self {
					OuterCall::TwoPhase(ref r) => Some(r),
					_ => None,
				}
			}
		}
		impl From<::frame_support::dispatch::CallableCallFor<TwoPhase, Runtime>> for OuterCall {
			fn from(call: ::frame_support::dispatch::CallableCallFor<TwoPhase, Runtime>) -> Self {
				OuterCall::TwoPhase(call)
			}
		}
		pub struct Origin {
			caller: OriginCaller,
			filter: ::frame_support::sp_std::rc::Rc<
				Box<dyn Fn(&<Runtime as frame_system::Trait>::Call) -> bool>,
			>,
		}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		impl ::core::clone::Clone for Origin {
			#[inline]
			fn clone(&self) -> Origin {
				match *self {
					Origin {
						caller: ref __self_0_0,
						filter: ref __self_0_1,
					} => Origin {
						caller: ::core::clone::Clone::clone(&(*__self_0_0)),
						filter: ::core::clone::Clone::clone(&(*__self_0_1)),
					},
				}
			}
		}
		#[cfg(feature = "std")]
		impl ::frame_support::sp_std::fmt::Debug for Origin {
			fn fmt(
				&self,
				fmt: &mut ::frame_support::sp_std::fmt::Formatter,
			) -> ::frame_support::sp_std::result::Result<(), ::frame_support::sp_std::fmt::Error> {
				fmt.debug_struct("Origin")
					.field("caller", &self.caller)
					.field("filter", &"[function ptr]")
					.finish()
			}
		}
		impl ::frame_support::traits::OriginTrait for Origin {
			type Call = <Runtime as frame_system::Trait>::Call;
			type PalletsOrigin = OriginCaller;
			type AccountId = <Runtime as frame_system::Trait>::AccountId;
			fn add_filter(&mut self, filter: impl Fn(&Self::Call) -> bool + 'static) {
				let f = self.filter.clone();
				self.filter = ::frame_support::sp_std::rc::Rc::new(Box::new(move |call| {
					f(call) && filter(call)
				}));
			}
			fn reset_filter(&mut self) {
				let filter = < < Runtime as frame_system :: Trait > :: BaseCallFilter as :: frame_support :: traits :: Filter < < Runtime as frame_system :: Trait > :: Call > > :: filter ;
				self.filter = ::frame_support::sp_std::rc::Rc::new(Box::new(filter));
			}
			fn set_caller_from(&mut self, other: impl Into<Self>) {
				self.caller = other.into().caller
			}
			fn filter_call(&self, call: &Self::Call) -> bool {
				(self.filter)(call)
			}
			fn caller(&self) -> &Self::PalletsOrigin {
				&self.caller
			}
			/// Create with system none origin and `frame-system::Trait::BaseCallFilter`.
			fn none() -> Self {
				frame_system::RawOrigin::None.into()
			}
			/// Create with system root origin and no filter.
			fn root() -> Self {
				frame_system::RawOrigin::Root.into()
			}
			/// Create with system signed origin and `frame-system::Trait::BaseCallFilter`.
			fn signed(by: <Runtime as frame_system::Trait>::AccountId) -> Self {
				frame_system::RawOrigin::Signed(by).into()
			}
		}
		#[allow(non_camel_case_types)]
		pub enum OriginCaller {
			system(frame_system::Origin<Runtime>),
			#[allow(dead_code)]
			Void(::frame_support::Void),
		}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		#[allow(non_camel_case_types)]
		impl ::core::clone::Clone for OriginCaller {
			#[inline]
			fn clone(&self) -> OriginCaller {
				match (&*self,) {
					(&OriginCaller::system(ref __self_0),) => {
						OriginCaller::system(::core::clone::Clone::clone(&(*__self_0)))
					}
					(&OriginCaller::Void(ref __self_0),) => {
						OriginCaller::Void(::core::clone::Clone::clone(&(*__self_0)))
					}
				}
			}
		}
		#[allow(non_camel_case_types)]
		impl ::core::marker::StructuralPartialEq for OriginCaller {}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		#[allow(non_camel_case_types)]
		impl ::core::cmp::PartialEq for OriginCaller {
			#[inline]
			fn eq(&self, other: &OriginCaller) -> bool {
				{
					let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
					let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
					if true && __self_vi == __arg_1_vi {
						match (&*self, &*other) {
							(
								&OriginCaller::system(ref __self_0),
								&OriginCaller::system(ref __arg_1_0),
							) => (*__self_0) == (*__arg_1_0),
							(
								&OriginCaller::Void(ref __self_0),
								&OriginCaller::Void(ref __arg_1_0),
							) => (*__self_0) == (*__arg_1_0),
							_ => unsafe { ::core::intrinsics::unreachable() },
						}
					} else {
						false
					}
				}
			}
			#[inline]
			fn ne(&self, other: &OriginCaller) -> bool {
				{
					let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
					let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
					if true && __self_vi == __arg_1_vi {
						match (&*self, &*other) {
							(
								&OriginCaller::system(ref __self_0),
								&OriginCaller::system(ref __arg_1_0),
							) => (*__self_0) != (*__arg_1_0),
							(
								&OriginCaller::Void(ref __self_0),
								&OriginCaller::Void(ref __arg_1_0),
							) => (*__self_0) != (*__arg_1_0),
							_ => unsafe { ::core::intrinsics::unreachable() },
						}
					} else {
						true
					}
				}
			}
		}
		#[allow(non_camel_case_types)]
		impl ::core::marker::StructuralEq for OriginCaller {}
		#[automatically_derived]
		#[allow(unused_qualifications)]
		#[allow(non_camel_case_types)]
		impl ::core::cmp::Eq for OriginCaller {
			#[inline]
			#[doc(hidden)]
			fn assert_receiver_is_total_eq(&self) -> () {
				{
					let _: ::core::cmp::AssertParamIsEq<frame_system::Origin<Runtime>>;
					let _: ::core::cmp::AssertParamIsEq<::frame_support::Void>;
				}
			}
		}
		impl core::fmt::Debug for OriginCaller {
			fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
				match self {
					Self::system(ref a0) => {
						fmt.debug_tuple("OriginCaller::system").field(a0).finish()
					}
					Self::Void(ref a0) => fmt.debug_tuple("OriginCaller::Void").field(a0).finish(),
					_ => Ok(()),
				}
			}
		}
		const _: () = {
			#[allow(unknown_lints)]
			#[allow(rust_2018_idioms)]
			extern crate codec as _parity_scale_codec;
			impl _parity_scale_codec::Encode for OriginCaller {
				fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {
					match *self {
						OriginCaller::system(ref aa) => {
							dest.push_byte(0usize as u8);
							dest.push(aa);
						}
						OriginCaller::Void(ref aa) => {
							dest.push_byte(1usize as u8);
							dest.push(aa);
						}
						_ => (),
					}
				}
			}
			impl _parity_scale_codec::EncodeLike for OriginCaller {}
		};
		const _: () = {
			#[allow(unknown_lints)]
			#[allow(rust_2018_idioms)]
			extern crate codec as _parity_scale_codec;
			impl _parity_scale_codec::Decode for OriginCaller {
				fn decode<DecIn: _parity_scale_codec::Input>(
					input: &mut DecIn,
				) -> core::result::Result<Self, _parity_scale_codec::Error> {
					match input.read_byte()? {
						x if x == 0usize as u8 => Ok(OriginCaller::system({
							let res = _parity_scale_codec::Decode::decode(input);
							match res {
								Err(_) => {
									return Err(
										"Error decoding field OriginCaller :: system.0".into()
									)
								}
								Ok(a) => a,
							}
						})),
						x if x == 1usize as u8 => Ok(OriginCaller::Void({
							let res = _parity_scale_codec::Decode::decode(input);
							match res {
								Err(_) => {
									return Err("Error decoding field OriginCaller :: Void.0".into())
								}
								Ok(a) => a,
							}
						})),
						x => Err("No such variant in enum OriginCaller".into()),
					}
				}
			}
		};
		#[allow(dead_code)]
		impl Origin {
			/// Create with system none origin and `frame-system::Trait::BaseCallFilter`.
			pub fn none() -> Self {
				<Origin as ::frame_support::traits::OriginTrait>::none()
			}
			/// Create with system root origin and no filter.
			pub fn root() -> Self {
				<Origin as ::frame_support::traits::OriginTrait>::root()
			}
			/// Create with system signed origin and `frame-system::Trait::BaseCallFilter`.
			pub fn signed(by: <Runtime as frame_system::Trait>::AccountId) -> Self {
				<Origin as ::frame_support::traits::OriginTrait>::signed(by)
			}
		}
		impl From<frame_system::Origin<Runtime>> for OriginCaller {
			fn from(x: frame_system::Origin<Runtime>) -> Self {
				OriginCaller::system(x)
			}
		}
		impl From<frame_system::Origin<Runtime>> for Origin {
			/// Convert to runtime origin:
			/// * root origin is built with no filter
			/// * others use `frame-system::Trait::BaseCallFilter`
			fn from(x: frame_system::Origin<Runtime>) -> Self {
				let o: OriginCaller = x.into();
				o.into()
			}
		}
		impl From<OriginCaller> for Origin {
			fn from(x: OriginCaller) -> Self {
				let mut o = Origin {
					caller: x,
					filter: ::frame_support::sp_std::rc::Rc::new(Box::new(|_| true)),
				};
				if !match o.caller {
					OriginCaller::system(frame_system::Origin::<Runtime>::Root) => true,
					_ => false,
				} {
					::frame_support::traits::OriginTrait::reset_filter(&mut o);
				}
				o
			}
		}
		impl Into<::frame_support::sp_std::result::Result<frame_system::Origin<Runtime>, Origin>>
			for Origin
		{
			/// NOTE: converting to pallet origin loses the origin filter information.
			fn into(
				self,
			) -> ::frame_support::sp_std::result::Result<frame_system::Origin<Runtime>, Self> {
				if let OriginCaller::system(l) = self.caller {
					Ok(l)
				} else {
					Err(self)
				}
			}
		}
		impl From<Option<<Runtime as frame_system::Trait>::AccountId>> for Origin {
			/// Convert to runtime origin with caller being system signed or none and use filter
			/// `frame-system::Trait::BaseCallFilter`.
			fn from(x: Option<<Runtime as frame_system::Trait>::AccountId>) -> Self {
				<frame_system::Origin<Runtime>>::from(x).into()
			}
		}
		impl frame_system::Trait for Runtime {
			type BaseCallFilter = ();
			type Origin = Origin;
			type Index = u64;
			type BlockNumber = u64;
			type Call = OuterCall;
			type Hash = H256;
			type Hashing = BlakeTwo256;
			type AccountId = AccountId;
			type Lookup = IdentityLookup<Self::AccountId>;
			type Header = Header;
			type Event = ();
			type BlockHashCount = ();
			type MaximumBlockWeight = ();
			type DbWeight = ();
			type BlockExecutionWeight = ();
			type ExtrinsicBaseWeight = ();
			type MaximumExtrinsicWeight = ();
			type MaximumBlockLength = ();
			type AvailableBlockRatio = ();
			type Version = ();
			type PalletInfo = ();
			type AccountData = pallet_balances::AccountData<u64>;
			type OnNewAccount = ();
			type OnKilledAccount = ();
			type SystemWeightInfo = ();
		}
		pub struct ExistentialDeposit;
		impl ExistentialDeposit {
			/// Returns the value of this parameter type.
			pub const fn get() -> u64 {
				1
			}
		}
		impl<I: From<u64>> ::frame_support::traits::Get<I> for ExistentialDeposit {
			fn get() -> I {
				I::from(1)
			}
		}
		impl pallet_balances::Trait for Runtime {
			type Balance = Balance;
			type Event = ();
			type DustRemoval = ();
			type ExistentialDeposit = ExistentialDeposit;
			type AccountStore = System;
			type MaxLocks = ();
			type WeightInfo = ();
		}
		use paste::paste;
		const SIGNED_PHASE: ::std::thread::LocalKey<RefCell<u64>> = {
			#[inline]
			fn __init() -> RefCell<u64> {
				RefCell::new(10)
			}
			unsafe fn __getit() -> ::std::option::Option<&'static RefCell<u64>> {
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<RefCell<u64>> =
					::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		const UNSIGNED_PHASE: ::std::thread::LocalKey<RefCell<u64>> = {
			#[inline]
			fn __init() -> RefCell<u64> {
				RefCell::new(5)
			}
			unsafe fn __getit() -> ::std::option::Option<&'static RefCell<u64>> {
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<RefCell<u64>> =
					::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		const MAX_SIGNED_SUBMISSIONS: ::std::thread::LocalKey<RefCell<u32>> = {
			#[inline]
			fn __init() -> RefCell<u32> {
				RefCell::new(5)
			}
			unsafe fn __getit() -> ::std::option::Option<&'static RefCell<u32>> {
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<RefCell<u32>> =
					::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		const TARGETS: ::std::thread::LocalKey<RefCell<Vec<AccountId>>> = {
			#[inline]
			fn __init() -> RefCell<Vec<AccountId>> {
				RefCell::new(<[_]>::into_vec(box [10, 20, 30, 40]))
			}
			unsafe fn __getit() -> ::std::option::Option<&'static RefCell<Vec<AccountId>>> {
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<RefCell<Vec<AccountId>>> =
					::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		const VOTERS: ::std::thread::LocalKey<
			RefCell<Vec<(AccountId, VoteWeight, Vec<AccountId>)>>,
		> = {
			#[inline]
			fn __init() -> RefCell<Vec<(AccountId, VoteWeight, Vec<AccountId>)>> {
				RefCell::new(<[_]>::into_vec(box [
					(1, 10, <[_]>::into_vec(box [10, 20])),
					(2, 10, <[_]>::into_vec(box [30, 40])),
					(3, 10, <[_]>::into_vec(box [40])),
					(4, 10, <[_]>::into_vec(box [10, 20, 30, 40])),
					(10, 10, <[_]>::into_vec(box [10])),
					(20, 20, <[_]>::into_vec(box [20])),
					(30, 30, <[_]>::into_vec(box [30])),
					(40, 40, <[_]>::into_vec(box [40])),
				]))
			}
			unsafe fn __getit(
			) -> ::std::option::Option<&'static RefCell<Vec<(AccountId, VoteWeight, Vec<AccountId>)>>>
			{
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<
					RefCell<Vec<(AccountId, VoteWeight, Vec<AccountId>)>>,
				> = ::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		const DESIRED_TARGETS: ::std::thread::LocalKey<RefCell<u32>> = {
			#[inline]
			fn __init() -> RefCell<u32> {
				RefCell::new(2)
			}
			unsafe fn __getit() -> ::std::option::Option<&'static RefCell<u32>> {
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<RefCell<u32>> =
					::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		const SIGNED_DEPOSIT_BASE: ::std::thread::LocalKey<RefCell<Balance>> = {
			#[inline]
			fn __init() -> RefCell<Balance> {
				RefCell::new(5)
			}
			unsafe fn __getit() -> ::std::option::Option<&'static RefCell<Balance>> {
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<RefCell<Balance>> =
					::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		const SIGNED_REWARD_BASE: ::std::thread::LocalKey<RefCell<Balance>> = {
			#[inline]
			fn __init() -> RefCell<Balance> {
				RefCell::new(7)
			}
			unsafe fn __getit() -> ::std::option::Option<&'static RefCell<Balance>> {
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<RefCell<Balance>> =
					::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		const MAX_UNSIGNED_ITERATIONS: ::std::thread::LocalKey<RefCell<u32>> = {
			#[inline]
			fn __init() -> RefCell<u32> {
				RefCell::new(5)
			}
			unsafe fn __getit() -> ::std::option::Option<&'static RefCell<u32>> {
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<RefCell<u32>> =
					::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		const UNSIGNED_PRIORITY: ::std::thread::LocalKey<RefCell<u64>> = {
			#[inline]
			fn __init() -> RefCell<u64> {
				RefCell::new(100)
			}
			unsafe fn __getit() -> ::std::option::Option<&'static RefCell<u64>> {
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<RefCell<u64>> =
					::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		const SOLUTION_IMPROVEMENT_THRESHOLD: ::std::thread::LocalKey<RefCell<Perbill>> = {
			#[inline]
			fn __init() -> RefCell<Perbill> {
				RefCell::new(Perbill::zero())
			}
			unsafe fn __getit() -> ::std::option::Option<&'static RefCell<Perbill>> {
				#[thread_local]
				#[cfg(all(
					target_thread_local,
					not(all(target_arch = "wasm32", not(target_feature = "atomics"))),
				))]
				static __KEY: ::std::thread::__FastLocalKeyInner<RefCell<Perbill>> =
					::std::thread::__FastLocalKeyInner::new();
				#[allow(unused_unsafe)]
				unsafe {
					__KEY.get(__init)
				}
			}
			unsafe { ::std::thread::LocalKey::new(__getit) }
		};
		pub struct SignedPhase;
		impl Get<u64> for SignedPhase {
			fn get() -> u64 {
				SIGNED_PHASE.with(|v| v.borrow().clone())
			}
		}
		pub struct UnsignedPhase;
		impl Get<u64> for UnsignedPhase {
			fn get() -> u64 {
				UNSIGNED_PHASE.with(|v| v.borrow().clone())
			}
		}
		pub struct MaxSignedSubmissions;
		impl Get<u32> for MaxSignedSubmissions {
			fn get() -> u32 {
				MAX_SIGNED_SUBMISSIONS.with(|v| v.borrow().clone())
			}
		}
		pub struct Targets;
		impl Get<Vec<AccountId>> for Targets {
			fn get() -> Vec<AccountId> {
				TARGETS.with(|v| v.borrow().clone())
			}
		}
		pub struct Voters;
		impl Get<Vec<(AccountId, VoteWeight, Vec<AccountId>)>> for Voters {
			fn get() -> Vec<(AccountId, VoteWeight, Vec<AccountId>)> {
				VOTERS.with(|v| v.borrow().clone())
			}
		}
		pub struct DesiredTargets;
		impl Get<u32> for DesiredTargets {
			fn get() -> u32 {
				DESIRED_TARGETS.with(|v| v.borrow().clone())
			}
		}
		pub struct SignedDepositBase;
		impl Get<Balance> for SignedDepositBase {
			fn get() -> Balance {
				SIGNED_DEPOSIT_BASE.with(|v| v.borrow().clone())
			}
		}
		pub struct SignedRewardBase;
		impl Get<Balance> for SignedRewardBase {
			fn get() -> Balance {
				SIGNED_REWARD_BASE.with(|v| v.borrow().clone())
			}
		}
		pub struct MaxUnsignedIterations;
		impl Get<u32> for MaxUnsignedIterations {
			fn get() -> u32 {
				MAX_UNSIGNED_ITERATIONS.with(|v| v.borrow().clone())
			}
		}
		pub struct UnsignedPriority;
		impl Get<u64> for UnsignedPriority {
			fn get() -> u64 {
				UNSIGNED_PRIORITY.with(|v| v.borrow().clone())
			}
		}
		pub struct SolutionImprovementThreshold;
		impl Get<Perbill> for SolutionImprovementThreshold {
			fn get() -> Perbill {
				SOLUTION_IMPROVEMENT_THRESHOLD.with(|v| v.borrow().clone())
			}
		}
		impl crate::two_phase::Trait for Runtime {
			type Event = ();
			type Currency = Balances;
			type SignedPhase = SignedPhase;
			type UnsignedPhase = UnsignedPhase;
			type MaxSignedSubmissions = MaxSignedSubmissions;
			type SignedRewardBase = SignedRewardBase;
			type SignedRewardFactor = ();
			type SignedRewardMax = ();
			type SignedDepositBase = SignedDepositBase;
			type SignedDepositByte = ();
			type SignedDepositWeight = ();
			type SolutionImprovementThreshold = SolutionImprovementThreshold;
			type SlashHandler = ();
			type RewardHandler = ();
			type UnsignedMaxIterations = MaxUnsignedIterations;
			type UnsignedPriority = UnsignedPriority;
			type ElectionDataProvider = StakingMock;
			type WeightInfo = ();
		}
		impl<LocalCall> frame_system::offchain::SendTransactionTypes<LocalCall> for Runtime
		where
			OuterCall: From<LocalCall>,
		{
			type OverarchingCall = OuterCall;
			type Extrinsic = Extrinsic;
		}
		pub type Extrinsic = sp_runtime::testing::TestXt<OuterCall, ()>;
		pub struct ExtBuilder {}
		impl Default for ExtBuilder {
			fn default() -> Self {
				Self {}
			}
		}
		pub struct StakingMock;
		impl ElectionDataProvider<AccountId, u64> for StakingMock {
			type CompactSolution = TestCompact;
			fn targets() -> Vec<AccountId> {
				Targets::get()
			}
			fn voters() -> Vec<(AccountId, VoteWeight, Vec<AccountId>)> {
				Voters::get()
			}
			fn desired_targets() -> u32 {
				DesiredTargets::get()
			}
			fn feasibility_check_assignment<P: PerThing>(
				_: &AccountId,
				_: &[(AccountId, P)],
			) -> bool {
				true
			}
			fn next_election_prediction(now: u64) -> u64 {
				now + 20 - now % 20
			}
		}
		impl ExtBuilder {
			pub fn max_signed_submission(self, count: u32) -> Self {
				MAX_SIGNED_SUBMISSIONS.with(|v| *v.borrow_mut() = count);
				self
			}
			pub fn unsigned_priority(self, p: u64) -> Self {
				UNSIGNED_PRIORITY.with(|v| *v.borrow_mut() = p);
				self
			}
			pub fn solution_improvement_threshold(self, p: Perbill) -> Self {
				SOLUTION_IMPROVEMENT_THRESHOLD.with(|v| *v.borrow_mut() = p);
				self
			}
			pub fn desired_targets(self, t: u32) -> Self {
				DESIRED_TARGETS.with(|v| *v.borrow_mut() = t);
				self
			}
			pub fn add_voter(
				self,
				who: AccountId,
				stake: Balance,
				targets: Vec<AccountId>,
			) -> Self {
				VOTERS.with(|v| v.borrow_mut().push((who, stake, targets)));
				self
			}
			pub fn build(self) -> sp_io::TestExternalities {
				sp_tracing::try_init_simple();
				let mut storage = frame_system::GenesisConfig::default()
					.build_storage::<Runtime>()
					.unwrap();
				let _ = pallet_balances::GenesisConfig::<Runtime> {
					balances: <[_]>::into_vec(box [(99, 100), (999, 100), (9999, 100)]),
				}
				.assimilate_storage(&mut storage);
				sp_io::TestExternalities::from(storage)
			}
			pub fn build_offchainify(
				self,
				iters: u32,
			) -> (sp_io::TestExternalities, Arc<RwLock<PoolState>>) {
				let mut ext = self.build();
				let (offchain, offchain_state) = TestOffchainExt::new();
				let (pool, pool_state) = TestTransactionPoolExt::new();
				let mut seed = [0_u8; 32];
				seed[0..4].copy_from_slice(&iters.to_le_bytes());
				offchain_state.write().seed = seed;
				ext.register_extension(OffchainExt::new(offchain));
				ext.register_extension(TransactionPoolExt::new(pool));
				(ext, pool_state)
			}
			pub fn build_and_execute(self, test: impl FnOnce() -> ()) {
				self.build().execute_with(test)
			}
		}
	}
	#[macro_use]
	pub(crate) mod macros {
		//! Some helper macros for this crate.
	}
	pub mod signed {
		//! The signed phase implementation.
		use crate::two_phase::*;
		use codec::Encode;
		use sp_arithmetic::traits::SaturatedConversion;
		use sp_npos_elections::is_score_better;
		use sp_runtime::Perbill;
		impl<T: Trait> Module<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			/// Start the signed phase.
			///
			/// Upon calling this, auxillary data for election is stored and signed solutions will be
			/// accepted.
			///
			/// The signed phase must always start before the unsigned phase.
			pub fn start_signed_phase() {
				let targets = T::ElectionDataProvider::targets();
				let voters = T::ElectionDataProvider::voters();
				let desired_targets = T::ElectionDataProvider::desired_targets();
				<SnapshotTargets<T>>::put(targets);
				<SnapshotVoters<T>>::put(voters);
				DesiredTargets::put(desired_targets);
			}
			/// Finish the singed phase. Process the signed submissions from best to worse until a valid one
			/// is found, rewarding the best oen and slashing the invalid ones along the way.
			///
			/// Returns true if we have a good solution in the signed phase.
			///
			/// This drains the [`SignedSubmissions`], potentially storing the best valid one in
			/// [`QueuedSolution`].
			pub fn finalize_signed_phase() -> bool {
				let mut all_submission: Vec<SignedSubmission<_, _, _>> =
					<SignedSubmissions<T>>::take();
				let mut found_solution = false;
				while let Some(best) = all_submission.pop() {
					let SignedSubmission {
						solution,
						who,
						deposit,
						reward,
					} = best;
					match Self::feasibility_check(solution, ElectionCompute::Signed) {
						Ok(ready_solution) => {
							<QueuedSolution<T>>::put(ready_solution);
							let _remaining = T::Currency::unreserve(&who, deposit);
							if true {
								if !_remaining.is_zero() {
									{
										::std::rt::begin_panic(
											"assertion failed: _remaining.is_zero()",
										)
									}
								};
							};
							let positive_imbalance = T::Currency::deposit_creating(&who, reward);
							T::RewardHandler::on_unbalanced(positive_imbalance);
							found_solution = true;
							break;
						}
						Err(_) => {
							let (negative_imbalance, _remaining) =
								T::Currency::slash_reserved(&who, deposit);
							if true {
								if !_remaining.is_zero() {
									{
										::std::rt::begin_panic(
											"assertion failed: _remaining.is_zero()",
										)
									}
								};
							};
							T::SlashHandler::on_unbalanced(negative_imbalance);
						}
					}
				}
				all_submission.into_iter().for_each(|not_processed| {
					let SignedSubmission { who, deposit, .. } = not_processed;
					let _remaining = T::Currency::unreserve(&who, deposit);
					if true {
						if !_remaining.is_zero() {
							{
								::std::rt::begin_panic("assertion failed: _remaining.is_zero()")
							}
						};
					};
				});
				found_solution
			}
			/// Find a proper position in the queue for the signed queue, whilst maintaining the order of
			/// solution quality.
			///
			/// The length of the queue will always be kept less than or equal to `T::MaxSignedSubmissions`.
			pub fn insert_submission(
				who: &T::AccountId,
				queue: &mut Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>,
				solution: RawSolution<CompactOf<T>>,
			) -> Option<usize> {
				let outcome = queue
					.iter()
					.enumerate()
					.rev()
					.find_map(|(i, s)| {
						if is_score_better::<Perbill>(
							solution.score,
							s.solution.score,
							T::SolutionImprovementThreshold::get(),
						) {
							Some(i + 1)
						} else {
							None
						}
					})
					.or(Some(0))
					.and_then(|at| {
						if at == 0 && queue.len() as u32 >= T::MaxSignedSubmissions::get() {
							None
						} else {
							let reward = Self::reward_for(&solution);
							let deposit = Self::deposit_for(&solution);
							let submission = SignedSubmission {
								who: who.clone(),
								deposit,
								reward,
								solution,
							};
							queue.insert(at, submission);
							if queue.len() as u32 > T::MaxSignedSubmissions::get() {
								queue.remove(0);
								Some(at - 1)
							} else {
								Some(at)
							}
						}
					});
				if true {
					if !(queue.len() as u32 <= T::MaxSignedSubmissions::get()) {
						{
							:: std :: rt :: begin_panic ( "assertion failed: queue.len() as u32 <= T::MaxSignedSubmissions::get()" )
						}
					};
				};
				outcome
			}
			/// Collect sufficient deposit to store this solution this chain.
			///
			/// The deposit is composed of 3 main elements:
			///
			/// 1. base deposit, fixed for all submissions.
			/// 2. a per-byte deposit, for renting the state usage.
			/// 3. a per-weight deposit, for the potential weight usage in an upcoming on_initialize
			pub fn deposit_for(solution: &RawSolution<CompactOf<T>>) -> BalanceOf<T> {
				let encoded_len: BalanceOf<T> = solution.using_encoded(|e| e.len() as u32).into();
				let feasibility_weight = T::WeightInfo::feasibility_check();
				let len_deposit = T::SignedDepositByte::get() * encoded_len;
				let weight_deposit =
					T::SignedDepositWeight::get() * feasibility_weight.saturated_into();
				T::SignedDepositBase::get() + len_deposit + weight_deposit
			}
			/// The reward for this solution, if successfully chosen as the best one at the end of the
			/// signed phase.
			pub fn reward_for(solution: &RawSolution<CompactOf<T>>) -> BalanceOf<T> {
				T::SignedRewardBase::get()
					+ T::SignedRewardFactor::get()
						* solution.score[0].saturated_into::<BalanceOf<T>>()
			}
		}
		#[cfg(test)]
		mod tests {
			use super::{mock::*, *};
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const cannot_submit_too_early: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName("two_phase::signed::tests::cannot_submit_too_early"),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| test::assert_test_result(cannot_submit_too_early())),
			};
			fn cannot_submit_too_early() {
				ExtBuilder::default().build_and_execute(|| {
					roll_to(2);
					{
						match (&TwoPhase::current_phase(), &Phase::Off) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					TwoPhase::start_signed_phase();
					let solution = raw_solution();
					let h = ::frame_support::storage_root();
					{
						match (
							&TwoPhase::submit(Origin::signed(10), solution),
							&Err(PalletError::<Runtime>::EarlySubmission.into()),
						) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&h, &::frame_support::storage_root()) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const should_pay_deposit: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName("two_phase::signed::tests::should_pay_deposit"),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| test::assert_test_result(should_pay_deposit())),
			};
			fn should_pay_deposit() {
				ExtBuilder::default().build_and_execute(|| {
					roll_to(5);
					{
						match (&TwoPhase::current_phase(), &Phase::Signed) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let solution = raw_solution();
					{
						match (&balances(&99), &(100, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let is = TwoPhase::submit(Origin::signed(99), solution);
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					{
						match (&balances(&99), &(95, 5)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&TwoPhase::signed_submissions().first().unwrap().deposit, &5) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const good_solution_is_rewarded: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName(
						"two_phase::signed::tests::good_solution_is_rewarded",
					),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(
					|| test::assert_test_result(good_solution_is_rewarded()),
				),
			};
			fn good_solution_is_rewarded() {
				ExtBuilder::default().build_and_execute(|| {
					roll_to(5);
					{
						match (&TwoPhase::current_phase(), &Phase::Signed) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let solution = raw_solution();
					{
						match (&balances(&99), &(100, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let is = TwoPhase::submit(Origin::signed(99), solution);
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					{
						match (&balances(&99), &(95, 5)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					if !TwoPhase::finalize_signed_phase() {
						{
							::std::rt::begin_panic(
								"assertion failed: TwoPhase::finalize_signed_phase()",
							)
						}
					};
					{
						match (&balances(&99), &(100 + 7, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const bad_solution_is_slashed: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName("two_phase::signed::tests::bad_solution_is_slashed"),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| test::assert_test_result(bad_solution_is_slashed())),
			};
			fn bad_solution_is_slashed() {
				ExtBuilder::default().build_and_execute(|| {
					roll_to(5);
					{
						match (&TwoPhase::current_phase(), &Phase::Signed) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let mut solution = raw_solution();
					{
						match (&balances(&99), &(100, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					solution.score[0] += 1;
					let is = TwoPhase::submit(Origin::signed(99), solution);
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					{
						match (&balances(&99), &(95, 5)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					if !!TwoPhase::finalize_signed_phase() {
						{
							::std::rt::begin_panic(
								"assertion failed: !TwoPhase::finalize_signed_phase()",
							)
						}
					};
					{
						match (&balances(&99), &(95, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const suppressed_solution_gets_bond_back: test::TestDescAndFn =
				test::TestDescAndFn {
					desc: test::TestDesc {
						name: test::StaticTestName(
							"two_phase::signed::tests::suppressed_solution_gets_bond_back",
						),
						ignore: false,
						allow_fail: false,
						should_panic: test::ShouldPanic::No,
						test_type: test::TestType::UnitTest,
					},
					testfn: test::StaticTestFn(|| {
						test::assert_test_result(suppressed_solution_gets_bond_back())
					}),
				};
			fn suppressed_solution_gets_bond_back() {
				ExtBuilder::default().build_and_execute(|| {
					roll_to(5);
					{
						match (&TwoPhase::current_phase(), &Phase::Signed) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let mut solution = raw_solution();
					{
						match (&balances(&99), &(100, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&balances(&999), &(100, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let is = TwoPhase::submit(Origin::signed(99), solution.clone());
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					solution.score[0] -= 1;
					let is = TwoPhase::submit(Origin::signed(999), solution);
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					{
						match (&balances(&99), &(95, 5)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&balances(&999), &(95, 5)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					if !TwoPhase::finalize_signed_phase() {
						{
							::std::rt::begin_panic(
								"assertion failed: TwoPhase::finalize_signed_phase()",
							)
						}
					};
					{
						match (&balances(&99), &(100 + 7, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&balances(&999), &(100, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const queue_is_always_sorted: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName("two_phase::signed::tests::queue_is_always_sorted"),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| test::assert_test_result(queue_is_always_sorted())),
			};
			fn queue_is_always_sorted() {
				ExtBuilder::default().build_and_execute(|| {
					roll_to(5);
					{
						match (&TwoPhase::current_phase(), &Phase::Signed) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let solution = RawSolution {
						score: [5, 0, 0],
						..Default::default()
					};
					let is = TwoPhase::submit(Origin::signed(99), solution);
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					let solution = RawSolution {
						score: [4, 0, 0],
						..Default::default()
					};
					let is = TwoPhase::submit(Origin::signed(99), solution);
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					let solution = RawSolution {
						score: [6, 0, 0],
						..Default::default()
					};
					let is = TwoPhase::submit(Origin::signed(99), solution);
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					{
						match (
							&TwoPhase::signed_submissions()
								.iter()
								.map(|x| x.solution.score[0])
								.collect::<Vec<_>>(),
							&<[_]>::into_vec(box [4, 5, 6]),
						) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const cannot_submit_worse_with_full_queue: test::TestDescAndFn =
				test::TestDescAndFn {
					desc: test::TestDesc {
						name: test::StaticTestName(
							"two_phase::signed::tests::cannot_submit_worse_with_full_queue",
						),
						ignore: false,
						allow_fail: false,
						should_panic: test::ShouldPanic::No,
						test_type: test::TestType::UnitTest,
					},
					testfn: test::StaticTestFn(|| {
						test::assert_test_result(cannot_submit_worse_with_full_queue())
					}),
				};
			fn cannot_submit_worse_with_full_queue() {
				ExtBuilder::default().build_and_execute(|| {
					roll_to(5);
					for s in 0..MaxSignedSubmissions::get() {
						let solution = RawSolution {
							score: [(5 + s).into(), 0, 0],
							..Default::default()
						};
						let is = TwoPhase::submit(Origin::signed(99), solution);
						match is {
							Ok(_) => (),
							_ => {
								if !false {
									{
										::std::rt::begin_panic_fmt(
											&::core::fmt::Arguments::new_v1_formatted(
												&["Expected Ok(_). Got "],
												&match (&is,) {
													(arg0,) => [::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													)],
												},
												&[::core::fmt::rt::v1::Argument {
													position: 0usize,
													format: ::core::fmt::rt::v1::FormatSpec {
														fill: ' ',
														align:
															::core::fmt::rt::v1::Alignment::Unknown,
														flags: 4u32,
														precision:
															::core::fmt::rt::v1::Count::Implied,
														width: ::core::fmt::rt::v1::Count::Implied,
													},
												}],
											),
										)
									}
								}
							}
						};
					}
					let solution = RawSolution {
						score: [4, 0, 0],
						..Default::default()
					};
					let h = ::frame_support::storage_root();
					{
						match (
							&TwoPhase::submit(Origin::signed(99), solution),
							&Err("QueueFull".into()),
						) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&h, &::frame_support::storage_root()) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const weakest_is_removed_if_better_provided: test::TestDescAndFn =
				test::TestDescAndFn {
					desc: test::TestDesc {
						name: test::StaticTestName(
							"two_phase::signed::tests::weakest_is_removed_if_better_provided",
						),
						ignore: false,
						allow_fail: false,
						should_panic: test::ShouldPanic::No,
						test_type: test::TestType::UnitTest,
					},
					testfn: test::StaticTestFn(|| {
						test::assert_test_result(weakest_is_removed_if_better_provided())
					}),
				};
			fn weakest_is_removed_if_better_provided() {
				ExtBuilder::default().build_and_execute(|| {
					roll_to(5);
					for s in 0..MaxSignedSubmissions::get() {
						let solution = RawSolution {
							score: [(5 + s).into(), 0, 0],
							..Default::default()
						};
						let is = TwoPhase::submit(Origin::signed(99), solution);
						match is {
							Ok(_) => (),
							_ => {
								if !false {
									{
										::std::rt::begin_panic_fmt(
											&::core::fmt::Arguments::new_v1_formatted(
												&["Expected Ok(_). Got "],
												&match (&is,) {
													(arg0,) => [::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													)],
												},
												&[::core::fmt::rt::v1::Argument {
													position: 0usize,
													format: ::core::fmt::rt::v1::FormatSpec {
														fill: ' ',
														align:
															::core::fmt::rt::v1::Alignment::Unknown,
														flags: 4u32,
														precision:
															::core::fmt::rt::v1::Count::Implied,
														width: ::core::fmt::rt::v1::Count::Implied,
													},
												}],
											),
										)
									}
								}
							}
						};
					}
					{
						match (
							&TwoPhase::signed_submissions()
								.into_iter()
								.map(|s| s.solution.score[0])
								.collect::<Vec<_>>(),
							&<[_]>::into_vec(box [5, 6, 7, 8, 9]),
						) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let solution = RawSolution {
						score: [20, 0, 0],
						..Default::default()
					};
					let is = TwoPhase::submit(Origin::signed(99), solution);
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					{
						match (
							&TwoPhase::signed_submissions()
								.into_iter()
								.map(|s| s.solution.score[0])
								.collect::<Vec<_>>(),
							&<[_]>::into_vec(box [6, 7, 8, 9, 20]),
						) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const equally_good_is_not_accepted: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName(
						"two_phase::signed::tests::equally_good_is_not_accepted",
					),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| {
					test::assert_test_result(equally_good_is_not_accepted())
				}),
			};
			fn equally_good_is_not_accepted() {
				ExtBuilder :: default ( ) . max_signed_submission ( 3 ) . build_and_execute ( | | { roll_to ( 5 ) ; for i in 0 .. MaxSignedSubmissions :: get ( ) { let solution = RawSolution { score : [ ( 5 + i ) . into ( ) , 0 , 0 ] , .. Default :: default ( ) } ; let is = TwoPhase :: submit ( Origin :: signed ( 99 ) , solution ) ; match is { Ok ( _ ) => ( ) , _ => if ! false { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1_formatted ( & [ "Expected Ok(_). Got " ] , & match ( & is , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } , & [ :: core :: fmt :: rt :: v1 :: Argument { position : 0usize , format : :: core :: fmt :: rt :: v1 :: FormatSpec { fill : ' ' , align : :: core :: fmt :: rt :: v1 :: Alignment :: Unknown , flags : 4u32 , precision : :: core :: fmt :: rt :: v1 :: Count :: Implied , width : :: core :: fmt :: rt :: v1 :: Count :: Implied , } , } ] ) ) } } , } ; } { match ( & TwoPhase :: signed_submissions ( ) . into_iter ( ) . map ( | s | s . solution . score [ 0 ] ) . collect :: < Vec < _ > > ( ) , & < [ _ ] > :: into_vec ( box [ 5 , 6 , 7 ] ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let solution = RawSolution { score : [ 5 , 0 , 0 ] , .. Default :: default ( ) } ; let h = :: frame_support :: storage_root ( ) ; { match ( & TwoPhase :: submit ( Origin :: signed ( 99 ) , solution ) , & Err ( "QueueFull" . into ( ) ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; { match ( & h , & :: frame_support :: storage_root ( ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; } )
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const solutions_are_always_sorted: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName(
						"two_phase::signed::tests::solutions_are_always_sorted",
					),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| {
					test::assert_test_result(solutions_are_always_sorted())
				}),
			};
			fn solutions_are_always_sorted() {
				ExtBuilder :: default ( ) . max_signed_submission ( 3 ) . build_and_execute ( | | { let scores = | | TwoPhase :: signed_submissions ( ) . into_iter ( ) . map ( | s | s . solution . score [ 0 ] ) . collect :: < Vec < _ > > ( ) ; roll_to ( 5 ) ; let solution = RawSolution { score : [ 5 , 0 , 0 ] , .. Default :: default ( ) } ; let is = TwoPhase :: submit ( Origin :: signed ( 99 ) , solution ) ; match is { Ok ( _ ) => ( ) , _ => if ! false { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1_formatted ( & [ "Expected Ok(_). Got " ] , & match ( & is , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } , & [ :: core :: fmt :: rt :: v1 :: Argument { position : 0usize , format : :: core :: fmt :: rt :: v1 :: FormatSpec { fill : ' ' , align : :: core :: fmt :: rt :: v1 :: Alignment :: Unknown , flags : 4u32 , precision : :: core :: fmt :: rt :: v1 :: Count :: Implied , width : :: core :: fmt :: rt :: v1 :: Count :: Implied , } , } ] ) ) } } , } ; { match ( & scores ( ) , & < [ _ ] > :: into_vec ( box [ 5 ] ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let solution = RawSolution { score : [ 8 , 0 , 0 ] , .. Default :: default ( ) } ; let is = TwoPhase :: submit ( Origin :: signed ( 99 ) , solution ) ; match is { Ok ( _ ) => ( ) , _ => if ! false { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1_formatted ( & [ "Expected Ok(_). Got " ] , & match ( & is , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } , & [ :: core :: fmt :: rt :: v1 :: Argument { position : 0usize , format : :: core :: fmt :: rt :: v1 :: FormatSpec { fill : ' ' , align : :: core :: fmt :: rt :: v1 :: Alignment :: Unknown , flags : 4u32 , precision : :: core :: fmt :: rt :: v1 :: Count :: Implied , width : :: core :: fmt :: rt :: v1 :: Count :: Implied , } , } ] ) ) } } , } ; { match ( & scores ( ) , & < [ _ ] > :: into_vec ( box [ 5 , 8 ] ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let solution = RawSolution { score : [ 3 , 0 , 0 ] , .. Default :: default ( ) } ; let is = TwoPhase :: submit ( Origin :: signed ( 99 ) , solution ) ; match is { Ok ( _ ) => ( ) , _ => if ! false { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1_formatted ( & [ "Expected Ok(_). Got " ] , & match ( & is , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } , & [ :: core :: fmt :: rt :: v1 :: Argument { position : 0usize , format : :: core :: fmt :: rt :: v1 :: FormatSpec { fill : ' ' , align : :: core :: fmt :: rt :: v1 :: Alignment :: Unknown , flags : 4u32 , precision : :: core :: fmt :: rt :: v1 :: Count :: Implied , width : :: core :: fmt :: rt :: v1 :: Count :: Implied , } , } ] ) ) } } , } ; { match ( & scores ( ) , & < [ _ ] > :: into_vec ( box [ 3 , 5 , 8 ] ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let solution = RawSolution { score : [ 6 , 0 , 0 ] , .. Default :: default ( ) } ; let is = TwoPhase :: submit ( Origin :: signed ( 99 ) , solution ) ; match is { Ok ( _ ) => ( ) , _ => if ! false { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1_formatted ( & [ "Expected Ok(_). Got " ] , & match ( & is , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } , & [ :: core :: fmt :: rt :: v1 :: Argument { position : 0usize , format : :: core :: fmt :: rt :: v1 :: FormatSpec { fill : ' ' , align : :: core :: fmt :: rt :: v1 :: Alignment :: Unknown , flags : 4u32 , precision : :: core :: fmt :: rt :: v1 :: Count :: Implied , width : :: core :: fmt :: rt :: v1 :: Count :: Implied , } , } ] ) ) } } , } ; { match ( & scores ( ) , & < [ _ ] > :: into_vec ( box [ 5 , 6 , 8 ] ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let solution = RawSolution { score : [ 6 , 0 , 0 ] , .. Default :: default ( ) } ; let is = TwoPhase :: submit ( Origin :: signed ( 99 ) , solution ) ; match is { Ok ( _ ) => ( ) , _ => if ! false { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1_formatted ( & [ "Expected Ok(_). Got " ] , & match ( & is , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } , & [ :: core :: fmt :: rt :: v1 :: Argument { position : 0usize , format : :: core :: fmt :: rt :: v1 :: FormatSpec { fill : ' ' , align : :: core :: fmt :: rt :: v1 :: Alignment :: Unknown , flags : 4u32 , precision : :: core :: fmt :: rt :: v1 :: Count :: Implied , width : :: core :: fmt :: rt :: v1 :: Count :: Implied , } , } ] ) ) } } , } ; { match ( & scores ( ) , & < [ _ ] > :: into_vec ( box [ 6 , 6 , 8 ] ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let solution = RawSolution { score : [ 10 , 0 , 0 ] , .. Default :: default ( ) } ; let is = TwoPhase :: submit ( Origin :: signed ( 99 ) , solution ) ; match is { Ok ( _ ) => ( ) , _ => if ! false { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1_formatted ( & [ "Expected Ok(_). Got " ] , & match ( & is , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } , & [ :: core :: fmt :: rt :: v1 :: Argument { position : 0usize , format : :: core :: fmt :: rt :: v1 :: FormatSpec { fill : ' ' , align : :: core :: fmt :: rt :: v1 :: Alignment :: Unknown , flags : 4u32 , precision : :: core :: fmt :: rt :: v1 :: Count :: Implied , width : :: core :: fmt :: rt :: v1 :: Count :: Implied , } , } ] ) ) } } , } ; { match ( & scores ( ) , & < [ _ ] > :: into_vec ( box [ 6 , 8 , 10 ] ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let solution = RawSolution { score : [ 12 , 0 , 0 ] , .. Default :: default ( ) } ; let is = TwoPhase :: submit ( Origin :: signed ( 99 ) , solution ) ; match is { Ok ( _ ) => ( ) , _ => if ! false { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1_formatted ( & [ "Expected Ok(_). Got " ] , & match ( & is , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } , & [ :: core :: fmt :: rt :: v1 :: Argument { position : 0usize , format : :: core :: fmt :: rt :: v1 :: FormatSpec { fill : ' ' , align : :: core :: fmt :: rt :: v1 :: Alignment :: Unknown , flags : 4u32 , precision : :: core :: fmt :: rt :: v1 :: Count :: Implied , width : :: core :: fmt :: rt :: v1 :: Count :: Implied , } , } ] ) ) } } , } ; { match ( & scores ( ) , & < [ _ ] > :: into_vec ( box [ 8 , 10 , 12 ] ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; } )
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const all_in_one_singed_submission_scenario: test::TestDescAndFn =
				test::TestDescAndFn {
					desc: test::TestDesc {
						name: test::StaticTestName(
							"two_phase::signed::tests::all_in_one_singed_submission_scenario",
						),
						ignore: false,
						allow_fail: false,
						should_panic: test::ShouldPanic::No,
						test_type: test::TestType::UnitTest,
					},
					testfn: test::StaticTestFn(|| {
						test::assert_test_result(all_in_one_singed_submission_scenario())
					}),
				};
			fn all_in_one_singed_submission_scenario() {
				ExtBuilder::default().build_and_execute(|| {
					roll_to(5);
					{
						match (&TwoPhase::current_phase(), &Phase::Signed) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&balances(&99), &(100, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&balances(&999), &(100, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&balances(&9999), &(100, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let mut solution = raw_solution();
					let is = TwoPhase::submit(Origin::signed(99), solution.clone());
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					solution.score[0] += 1;
					let is = TwoPhase::submit(Origin::signed(999), solution.clone());
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					solution.score[0] -= 1;
					let is = TwoPhase::submit(Origin::signed(9999), solution);
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					{
						match (
							&TwoPhase::signed_submissions()
								.iter()
								.map(|x| x.who)
								.collect::<Vec<_>>(),
							&<[_]>::into_vec(box [9999, 99, 999]),
						) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					if !TwoPhase::finalize_signed_phase() {
						{
							::std::rt::begin_panic(
								"assertion failed: TwoPhase::finalize_signed_phase()",
							)
						}
					};
					{
						match (&balances(&99), &(100 + 7, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&balances(&999), &(95, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					{
						match (&balances(&9999), &(100, 0)) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
				})
			}
		}
	}
	pub mod unsigned {
		//! The unsigned phase implementation.
		use crate::two_phase::*;
		use frame_support::{dispatch::DispatchResult, unsigned::ValidateUnsigned};
		use frame_system::offchain::SubmitTransaction;
		use sp_npos_elections::{seq_phragmen, CompactSolution, ElectionResult};
		use sp_runtime::{
			offchain::storage::StorageValueRef,
			traits::TrailingZeroInput,
			transaction_validity::{
				InvalidTransaction, TransactionSource, TransactionValidity,
				TransactionValidityError, ValidTransaction,
			},
			SaturatedConversion,
		};
		use sp_std::cmp::Ordering;
		/// Storage key used to store the persistent offchain worker status.
		pub(crate) const OFFCHAIN_HEAD_DB: &[u8] = b"parity/unsigned-election/";
		/// The repeat threshold of the offchain worker. This means we won't run the offchain worker twice
		/// within a window of 5 blocks.
		pub(crate) const OFFCHAIN_REPEAT: u32 = 5;
		/// Default number of blocks for which the unsigned transaction should stay in the pool
		pub(crate) const DEFAULT_LONGEVITY: u64 = 25;
		impl<T: Trait> Module<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			/// Min a new npos solution.
			pub fn mine_solution(iters: usize) -> Result<RawSolution<CompactOf<T>>, Error> {
				let desired_targets = Self::desired_targets() as usize;
				let voters = Self::snapshot_voters().ok_or(Error::SnapshotUnAvailable)?;
				let targets = Self::snapshot_targets().ok_or(Error::SnapshotUnAvailable)?;
				seq_phragmen::<_, CompactAccuracyOf<T>>(
					desired_targets,
					targets,
					voters,
					Some((iters, 0)),
				)
				.map_err(Into::into)
				.and_then(Self::prepare_election_result)
			}
			/// Convert a raw solution from [`sp_npos_elections::ElectionResult`] to [`RawSolution`], which
			/// is ready to be submitted to the chain.
			///
			/// Will always reduce the solution as well.
			pub fn prepare_election_result(
				election_result: ElectionResult<T::AccountId, CompactAccuracyOf<T>>,
			) -> Result<RawSolution<CompactOf<T>>, Error> {
				let voters = Self::snapshot_voters().ok_or(Error::SnapshotUnAvailable)?;
				let targets = Self::snapshot_targets().ok_or(Error::SnapshotUnAvailable)?;
				let voter_index =
					|who: &T::AccountId| -> Option<crate::two_phase::CompactVoterIndexOf<T>> {
						voters . iter ( ) . position ( | ( x , _ , _ ) | x == who ) . and_then ( | i | < usize as crate :: TryInto < crate :: two_phase :: CompactVoterIndexOf < T > > > :: try_into ( i ) . ok ( ) )
					};
				let target_index =
					|who: &T::AccountId| -> Option<crate::two_phase::CompactTargetIndexOf<T>> {
						targets . iter ( ) . position ( | x | x == who ) . and_then ( | i | < usize as crate :: TryInto < crate :: two_phase :: CompactTargetIndexOf < T > > > :: try_into ( i ) . ok ( ) )
					};
				let voter_at =
					|i: crate::two_phase::CompactVoterIndexOf<T>| -> Option<T::AccountId> {
						< crate :: two_phase :: CompactVoterIndexOf < T > as crate :: TryInto < usize > > :: try_into ( i ) . ok ( ) . and_then ( | i | voters . get ( i ) . map ( | ( x , _ , _ ) | x ) . cloned ( ) )
					};
				let target_at =
					|i: crate::two_phase::CompactTargetIndexOf<T>| -> Option<T::AccountId> {
						< crate :: two_phase :: CompactTargetIndexOf < T > as crate :: TryInto < usize > > :: try_into ( i ) . ok ( ) . and_then ( | i | targets . get ( i ) . cloned ( ) )
					};
				let stake_of = |who: &T::AccountId| -> crate::VoteWeight {
					voters
						.iter()
						.find(|(x, _, _)| x == who)
						.map(|(_, x, _)| *x)
						.unwrap_or_default()
				};
				let ElectionResult {
					assignments,
					winners,
				} = election_result;
				let mut staked = sp_npos_elections::assignment_ratio_to_staked_normalized(
					assignments,
					&stake_of,
				)
				.map_err::<Error, _>(Into::into)?;
				sp_npos_elections::reduce(&mut staked);
				let ratio = sp_npos_elections::assignment_staked_to_ratio_normalized(staked)?;
				let compact = <CompactOf<T>>::from_assignment(ratio, &voter_index, &target_index)?;
				let maximum_allowed_voters =
					Self::maximum_compact_len::<T::WeightInfo>(0, Default::default(), 0);
				let compact = Self::trim_compact(compact.len() as u32, compact, &voter_index)?;
				let winners = sp_npos_elections::to_without_backing(winners);
				let score = compact
					.clone()
					.score(&winners, stake_of, voter_at, target_at)?;
				Ok(RawSolution { compact, score })
			}
			/// Get a random number of iterations to run the balancing in the OCW.
			///
			/// Uses the offchain seed to generate a random number, maxed with `T::UnsignedMaxIterations`.
			pub fn get_balancing_iters() -> usize {
				match T::UnsignedMaxIterations::get() {
					0 => 0,
					max @ _ => {
						let seed = sp_io::offchain::random_seed();
						let random = <u32>::decode(&mut TrailingZeroInput::new(seed.as_ref()))
							.expect("input is padded with zeroes; qed")
							% max.saturating_add(1);
						random as usize
					}
				}
			}
			/// Greedily reduce the size of the a solution to fit into the block, w.r.t. weight.
			///
			/// The weight of the solution is foremost a function of the number of voters (i.e.
			/// `compact.len()`). Aside from this, the other components of the weight are invariant. The
			/// number of winners shall not be changed (otherwise the solution is invalid) and the
			/// `ElectionSize` is merely a representation of the total number of stakers.
			///
			/// Thus, we reside to stripping away some voters. This means only changing the `compact`
			/// struct.
			///
			/// Note that the solution is already computed, and the winners are elected based on the merit
			/// of teh entire stake in the system. Nonetheless, some of the voters will be removed further
			/// down the line.
			///
			/// Indeed, the score must be computed **after** this step. If this step reduces the score too
			/// much, then the solution will be discarded.
			pub fn trim_compact<FN>(
				maximum_allowed_voters: u32,
				mut compact: CompactOf<T>,
				nominator_index: FN,
			) -> Result<CompactOf<T>, Error>
			where
				for<'r> FN: Fn(&'r T::AccountId) -> Option<CompactVoterIndexOf<T>>,
			{
				match compact.len().checked_sub(maximum_allowed_voters as usize) {
					Some(to_remove) if to_remove > 0 => {
						let voters = Self::snapshot_voters().ok_or(Error::SnapshotUnAvailable)?;
						let mut voters_sorted = voters
							.into_iter()
							.map(|(who, stake, _)| (who.clone(), stake))
							.collect::<Vec<_>>();
						voters_sorted.sort_by_key(|(_, y)| *y);
						let mut removed = 0;
						for (maybe_index, _stake) in voters_sorted
							.iter()
							.map(|(who, stake)| (nominator_index(&who), stake))
						{
							let index = maybe_index.ok_or(Error::SnapshotUnAvailable)?;
							if compact.remove_voter(index) {
								removed += 1
							}
							if removed >= to_remove {
								break;
							}
						}
						Ok(compact)
					}
					_ => Ok(compact),
				}
			}
			/// Find the maximum `len` that a compact can have in order to fit into the block weight.
			///
			/// This only returns a value between zero and `size.nominators`.
			pub fn maximum_compact_len<W: WeightInfo>(
				_winners_len: u32,
				witness: WitnessData,
				max_weight: Weight,
			) -> u32 {
				if witness.voters < 1 {
					return witness.voters;
				}
				let max_voters = witness.voters.max(1);
				let mut voters = max_voters;
				let weight_with = |_voters: u32| -> Weight { W::submit_unsigned() };
				let next_voters =
					|current_weight: Weight, voters: u32, step: u32| -> Result<u32, ()> {
						match current_weight.cmp(&max_weight) {
							Ordering::Less => {
								let next_voters = voters.checked_add(step);
								match next_voters {
									Some(voters) if voters < max_voters => Ok(voters),
									_ => Err(()),
								}
							}
							Ordering::Greater => voters.checked_sub(step).ok_or(()),
							Ordering::Equal => Ok(voters),
						}
					};
				let mut step = voters / 2;
				let mut current_weight = weight_with(voters);
				while step > 0 {
					match next_voters(current_weight, voters, step) {
						Ok(next) if next != voters => {
							voters = next;
						}
						Err(()) => {
							break;
						}
						Ok(next) => return next,
					}
					step = step / 2;
					current_weight = weight_with(voters);
				}
				while voters + 1 <= max_voters && weight_with(voters + 1) < max_weight {
					voters += 1;
				}
				while voters.checked_sub(1).is_some() && weight_with(voters) > max_weight {
					voters -= 1;
				}
				if true {
					if !(weight_with(voters.min(witness.voters)) <= max_weight) {
						{
							::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
								&["weight_with(", ") <= "],
								&match (&voters.min(witness.voters), &max_weight) {
									(arg0, arg1) => [
										::core::fmt::ArgumentV1::new(
											arg0,
											::core::fmt::Display::fmt,
										),
										::core::fmt::ArgumentV1::new(
											arg1,
											::core::fmt::Display::fmt,
										),
									],
								},
							))
						}
					};
				};
				voters.min(witness.voters)
			}
			/// Checks if an execution of the offchain worker is permitted at the given block number, or not.
			///
			/// This essentially makes sure that we don't run on previous blocks in case of a re-org, and we
			/// don't run twice within a window of length [`OFFCHAIN_REPEAT`].
			///
			/// Returns `Ok(())` if offchain worker should happen, `Err(reason)` otherwise.
			pub(crate) fn set_check_offchain_execution_status(
				now: T::BlockNumber,
			) -> Result<(), &'static str> {
				let storage = StorageValueRef::persistent(&OFFCHAIN_HEAD_DB);
				let threshold = T::BlockNumber::from(OFFCHAIN_REPEAT);
				let mutate_stat = storage.mutate::<_, &'static str, _>(
					|maybe_head: Option<Option<T::BlockNumber>>| match maybe_head {
						Some(Some(head)) if now < head => Err("fork."),
						Some(Some(head)) if now >= head && now <= head + threshold => {
							Err("recently executed.")
						}
						Some(Some(head)) if now > head + threshold => Ok(now),
						_ => Ok(now),
					},
				);
				match mutate_stat {
					Ok(Ok(_)) => Ok(()),
					Ok(Err(_)) => Err("failed to write to offchain db."),
					Err(why) => Err(why),
				}
			}
			/// Mine a new solution, and submit it back to the chian as an unsigned transaction.
			pub(crate) fn mine_and_submit() -> Result<(), Error> {
				let balancing = Self::get_balancing_iters();
				Self::mine_solution(balancing).and_then(|raw_solution| {
					let call = Call::submit_unsigned(raw_solution).into();
					SubmitTransaction::<T, Call<T>>::submit_unsigned_transaction(call)
						.map_err(|_| Error::PoolSubmissionFailed)
				})
			}
			pub(crate) fn pre_dispatch_checks(
				solution: &RawSolution<CompactOf<T>>,
			) -> DispatchResult {
				{
					if !Self::current_phase().is_unsigned_open() {
						{
							return Err(PalletError::<T>::EarlySubmission.into());
						};
					}
				};
				{
					if !Self::queued_solution().map_or(true, |q: ReadySolution<_>| {
						is_score_better::<Perbill>(
							solution.score,
							q.score,
							T::SolutionImprovementThreshold::get(),
						)
					}) {
						{
							return Err(PalletError::<T>::WeakSubmission.into());
						};
					}
				};
				Ok(())
			}
		}
		#[allow(deprecated)]
		impl<T: Trait> ValidateUnsigned for Module<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		{
			type Call = Call<T>;
			fn validate_unsigned(
				source: TransactionSource,
				call: &Self::Call,
			) -> TransactionValidity {
				if let Call::submit_unsigned(solution) = call {
					match source {
						TransactionSource::Local | TransactionSource::InBlock => {}
						_ => {
							return InvalidTransaction::Call.into();
						}
					}
					if let Err(_why) = Self::pre_dispatch_checks(solution) {
						return InvalidTransaction::Custom(99).into();
					}
					ValidTransaction::with_tag_prefix("OffchainElection")
						.priority(
							T::UnsignedPriority::get()
								.saturating_add(solution.score[0].saturated_into()),
						)
						.longevity(DEFAULT_LONGEVITY)
						.propagate(false)
						.build()
				} else {
					InvalidTransaction::Call.into()
				}
			}
			fn pre_dispatch(call: &Self::Call) -> Result<(), TransactionValidityError> {
				if let Call::submit_unsigned(solution) = call {
					Self::pre_dispatch_checks(solution)
						.map_err(|_| InvalidTransaction::Custom(99).into())
				} else {
					Err(InvalidTransaction::Call.into())
				}
			}
		}
		#[cfg(test)]
		mod tests {
			use super::{mock::*, *};
			use frame_support::traits::OffchainWorker;
			use sp_runtime::PerU16;
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const validate_unsigned_retracts_wrong_phase: test::TestDescAndFn =
				test::TestDescAndFn {
					desc: test::TestDesc {
						name: test::StaticTestName(
							"two_phase::unsigned::tests::validate_unsigned_retracts_wrong_phase",
						),
						ignore: false,
						allow_fail: false,
						should_panic: test::ShouldPanic::No,
						test_type: test::TestType::UnitTest,
					},
					testfn: test::StaticTestFn(|| {
						test::assert_test_result(validate_unsigned_retracts_wrong_phase())
					}),
				};
			fn validate_unsigned_retracts_wrong_phase() {
				ExtBuilder :: default ( ) . build_and_execute ( | | { let solution = RawSolution :: < TestCompact > { score : [ 5 , 0 , 0 ] , .. Default :: default ( ) } ; let call = Call :: submit_unsigned ( solution . clone ( ) ) ; { match ( & TwoPhase :: current_phase ( ) , & Phase :: Off ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; match < TwoPhase as ValidateUnsigned > :: validate_unsigned ( TransactionSource :: Local , & call ) . unwrap_err ( ) { TransactionValidityError :: Invalid ( InvalidTransaction :: Custom ( 99 ) ) => true , _ => false , } ; match < TwoPhase as ValidateUnsigned > :: pre_dispatch ( & call ) . unwrap_err ( ) { TransactionValidityError :: Invalid ( InvalidTransaction :: Custom ( 99 ) ) => true , _ => false , } ; roll_to ( 5 ) ; { match ( & TwoPhase :: current_phase ( ) , & Phase :: Signed ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; match < TwoPhase as ValidateUnsigned > :: validate_unsigned ( TransactionSource :: Local , & call ) . unwrap_err ( ) { TransactionValidityError :: Invalid ( InvalidTransaction :: Custom ( 99 ) ) => true , _ => false , } ; match < TwoPhase as ValidateUnsigned > :: pre_dispatch ( & call ) . unwrap_err ( ) { TransactionValidityError :: Invalid ( InvalidTransaction :: Custom ( 99 ) ) => true , _ => false , } ; roll_to ( 15 ) ; { match ( & TwoPhase :: current_phase ( ) , & Phase :: Unsigned ( ( true , 15 ) ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; if ! < TwoPhase as ValidateUnsigned > :: validate_unsigned ( TransactionSource :: Local , & call ) . is_ok ( ) { { :: std :: rt :: begin_panic ( "assertion failed: <TwoPhase as\n    ValidateUnsigned>::validate_unsigned(TransactionSource::Local,\n                                         &call).is_ok()" ) } } ; if ! < TwoPhase as ValidateUnsigned > :: pre_dispatch ( & call ) . is_ok ( ) { { :: std :: rt :: begin_panic ( "assertion failed: <TwoPhase as ValidateUnsigned>::pre_dispatch(&call).is_ok()" ) } } ; } )
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const validate_unsigned_retracts_low_score: test::TestDescAndFn =
				test::TestDescAndFn {
					desc: test::TestDesc {
						name: test::StaticTestName(
							"two_phase::unsigned::tests::validate_unsigned_retracts_low_score",
						),
						ignore: false,
						allow_fail: false,
						should_panic: test::ShouldPanic::No,
						test_type: test::TestType::UnitTest,
					},
					testfn: test::StaticTestFn(|| {
						test::assert_test_result(validate_unsigned_retracts_low_score())
					}),
				};
			fn validate_unsigned_retracts_low_score() {
				ExtBuilder :: default ( ) . build_and_execute ( | | { roll_to ( 15 ) ; { match ( & TwoPhase :: current_phase ( ) , & Phase :: Unsigned ( ( true , 15 ) ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let solution = RawSolution :: < TestCompact > { score : [ 5 , 0 , 0 ] , .. Default :: default ( ) } ; let call = Call :: submit_unsigned ( solution . clone ( ) ) ; if ! < TwoPhase as ValidateUnsigned > :: validate_unsigned ( TransactionSource :: Local , & call ) . is_ok ( ) { { :: std :: rt :: begin_panic ( "assertion failed: <TwoPhase as\n    ValidateUnsigned>::validate_unsigned(TransactionSource::Local,\n                                         &call).is_ok()" ) } } ; if ! < TwoPhase as ValidateUnsigned > :: pre_dispatch ( & call ) . is_ok ( ) { { :: std :: rt :: begin_panic ( "assertion failed: <TwoPhase as ValidateUnsigned>::pre_dispatch(&call).is_ok()" ) } } ; let ready = ReadySolution { score : [ 10 , 0 , 0 ] , .. Default :: default ( ) } ; < QueuedSolution < Runtime > > :: put ( ready ) ; match < TwoPhase as ValidateUnsigned > :: validate_unsigned ( TransactionSource :: Local , & call ) . unwrap_err ( ) { TransactionValidityError :: Invalid ( InvalidTransaction :: Custom ( 99 ) ) => true , _ => false , } ; match < TwoPhase as ValidateUnsigned > :: pre_dispatch ( & call ) . unwrap_err ( ) { TransactionValidityError :: Invalid ( InvalidTransaction :: Custom ( 99 ) ) => true , _ => false , } ; } )
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const priority_is_set: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName("two_phase::unsigned::tests::priority_is_set"),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| test::assert_test_result(priority_is_set())),
			};
			fn priority_is_set() {
				ExtBuilder :: default ( ) . unsigned_priority ( 20 ) . build_and_execute ( | | { roll_to ( 15 ) ; { match ( & TwoPhase :: current_phase ( ) , & Phase :: Unsigned ( ( true , 15 ) ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let solution = RawSolution :: < TestCompact > { score : [ 5 , 0 , 0 ] , .. Default :: default ( ) } ; let call = Call :: submit_unsigned ( solution . clone ( ) ) ; { match ( & < TwoPhase as ValidateUnsigned > :: validate_unsigned ( TransactionSource :: Local , & call ) . unwrap ( ) . priority , & 25 ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; } )
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const invalid_solution_panics : test :: TestDescAndFn = test :: TestDescAndFn { desc : test :: TestDesc { name : test :: StaticTestName ( "two_phase::unsigned::tests::invalid_solution_panics" ) , ignore : false , allow_fail : false , should_panic : test :: ShouldPanic :: YesWithMessage ( "Invalid unsigned submission must produce invalid block and deprive validator from their authoring reward.: FeasibilityError::WrongWinnerCount" ) , test_type : test :: TestType :: UnitTest , } , testfn : test :: StaticTestFn ( | | test :: assert_test_result ( invalid_solution_panics ( ) ) ) , } ;
			#[should_panic(
				expected = "Invalid unsigned submission must produce invalid block and deprive \
		validator from their authoring reward.: FeasibilityError::WrongWinnerCount"
			)]
			fn invalid_solution_panics() {
				ExtBuilder::default().build_and_execute(|| {
					use frame_support::dispatch::Dispatchable;
					roll_to(15);
					{
						match (&TwoPhase::current_phase(), &Phase::Unsigned((true, 15))) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let solution = RawSolution::<TestCompact> {
						score: [5, 0, 0],
						..Default::default()
					};
					let call = Call::submit_unsigned(solution.clone());
					let outer_call: OuterCall = call.into();
					let _ = outer_call.dispatch(Origin::none());
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const miner_works: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName("two_phase::unsigned::tests::miner_works"),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| test::assert_test_result(miner_works())),
			};
			fn miner_works() {
				ExtBuilder::default().build_and_execute(|| {
					roll_to(15);
					{
						match (&TwoPhase::current_phase(), &Phase::Unsigned((true, 15))) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					if !TwoPhase::snapshot_voters().is_some() {
						{
							::std::rt::begin_panic(
								"assertion failed: TwoPhase::snapshot_voters().is_some()",
							)
						}
					};
					if !TwoPhase::snapshot_targets().is_some() {
						{
							::std::rt::begin_panic(
								"assertion failed: TwoPhase::snapshot_targets().is_some()",
							)
						}
					};
					{
						match (&TwoPhase::desired_targets(), &2) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let solution = TwoPhase::mine_solution(2).unwrap();
					if !TwoPhase::queued_solution().is_none() {
						{
							::std::rt::begin_panic(
								"assertion failed: TwoPhase::queued_solution().is_none()",
							)
						}
					};
					let is = TwoPhase::submit_unsigned(Origin::none(), solution);
					match is {
						Ok(_) => (),
						_ => {
							if !false {
								{
									::std::rt::begin_panic_fmt(
										&::core::fmt::Arguments::new_v1_formatted(
											&["Expected Ok(_). Got "],
											&match (&is,) {
												(arg0,) => [::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												)],
											},
											&[::core::fmt::rt::v1::Argument {
												position: 0usize,
												format: ::core::fmt::rt::v1::FormatSpec {
													fill: ' ',
													align: ::core::fmt::rt::v1::Alignment::Unknown,
													flags: 4u32,
													precision: ::core::fmt::rt::v1::Count::Implied,
													width: ::core::fmt::rt::v1::Count::Implied,
												},
											}],
										),
									)
								}
							}
						}
					};
					if !TwoPhase::queued_solution().is_some() {
						{
							::std::rt::begin_panic(
								"assertion failed: TwoPhase::queued_solution().is_some()",
							)
						}
					};
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const ocw_will_only_submit_if_feasible: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName(
						"two_phase::unsigned::tests::ocw_will_only_submit_if_feasible",
					),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| {
					test::assert_test_result(ocw_will_only_submit_if_feasible())
				}),
			};
			fn ocw_will_only_submit_if_feasible() {}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const can_only_submit_threshold_better: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName(
						"two_phase::unsigned::tests::can_only_submit_threshold_better",
					),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| {
					test::assert_test_result(can_only_submit_threshold_better())
				}),
			};
			fn can_only_submit_threshold_better() {
				ExtBuilder :: default ( ) . desired_targets ( 1 ) . add_voter ( 7 , 2 , < [ _ ] > :: into_vec ( box [ 10 ] ) ) . add_voter ( 8 , 5 , < [ _ ] > :: into_vec ( box [ 10 ] ) ) . solution_improvement_threshold ( Perbill :: from_percent ( 50 ) ) . build_and_execute ( | | { roll_to ( 15 ) ; { match ( & TwoPhase :: current_phase ( ) , & Phase :: Unsigned ( ( true , 15 ) ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; { match ( & TwoPhase :: desired_targets ( ) , & 1 ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let result = ElectionResult { winners : < [ _ ] > :: into_vec ( box [ ( 10 , 10 ) ] ) , assignments : < [ _ ] > :: into_vec ( box [ Assignment { who : 10 , distribution : < [ _ ] > :: into_vec ( box [ ( 10 , PerU16 :: one ( ) ) ] ) , } ] ) , } ; let is = TwoPhase :: submit_unsigned ( Origin :: none ( ) , TwoPhase :: prepare_election_result ( result ) . unwrap ( ) ) ; match is { Ok ( _ ) => ( ) , _ => if ! false { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1_formatted ( & [ "Expected Ok(_). Got " ] , & match ( & is , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } , & [ :: core :: fmt :: rt :: v1 :: Argument { position : 0usize , format : :: core :: fmt :: rt :: v1 :: FormatSpec { fill : ' ' , align : :: core :: fmt :: rt :: v1 :: Alignment :: Unknown , flags : 4u32 , precision : :: core :: fmt :: rt :: v1 :: Count :: Implied , width : :: core :: fmt :: rt :: v1 :: Count :: Implied , } , } ] ) ) } } , } ; { match ( & TwoPhase :: queued_solution ( ) . unwrap ( ) . score [ 0 ] , & 10 ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let result = ElectionResult { winners : < [ _ ] > :: into_vec ( box [ ( 10 , 12 ) ] ) , assignments : < [ _ ] > :: into_vec ( box [ Assignment { who : 10 , distribution : < [ _ ] > :: into_vec ( box [ ( 10 , PerU16 :: one ( ) ) ] ) , } , Assignment { who : 7 , distribution : < [ _ ] > :: into_vec ( box [ ( 10 , PerU16 :: one ( ) ) ] ) , } ] ) , } ; let solution = TwoPhase :: prepare_election_result ( result ) . unwrap ( ) ; { match ( & solution . score [ 0 ] , & 12 ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let h = :: frame_support :: storage_root ( ) ; { match ( & TwoPhase :: submit_unsigned ( Origin :: none ( ) , solution ) , & Err ( PalletError :: < Runtime > :: WeakSubmission . into ( ) ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; { match ( & h , & :: frame_support :: storage_root ( ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let result = ElectionResult { winners : < [ _ ] > :: into_vec ( box [ ( 10 , 12 ) ] ) , assignments : < [ _ ] > :: into_vec ( box [ Assignment { who : 10 , distribution : < [ _ ] > :: into_vec ( box [ ( 10 , PerU16 :: one ( ) ) ] ) , } , Assignment { who : 7 , distribution : < [ _ ] > :: into_vec ( box [ ( 10 , PerU16 :: one ( ) ) ] ) , } , Assignment { who : 8 , distribution : < [ _ ] > :: into_vec ( box [ ( 10 , PerU16 :: one ( ) ) ] ) , } ] ) , } ; let solution = TwoPhase :: prepare_election_result ( result ) . unwrap ( ) ; { match ( & solution . score [ 0 ] , & 17 ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; let is = TwoPhase :: submit_unsigned ( Origin :: none ( ) , solution ) ; match is { Ok ( _ ) => ( ) , _ => if ! false { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1_formatted ( & [ "Expected Ok(_). Got " ] , & match ( & is , ) { ( arg0 , ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) ] , } , & [ :: core :: fmt :: rt :: v1 :: Argument { position : 0usize , format : :: core :: fmt :: rt :: v1 :: FormatSpec { fill : ' ' , align : :: core :: fmt :: rt :: v1 :: Alignment :: Unknown , flags : 4u32 , precision : :: core :: fmt :: rt :: v1 :: Count :: Implied , width : :: core :: fmt :: rt :: v1 :: Count :: Implied , } , } ] ) ) } } , } ; } )
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const ocw_check_prevent_duplicate: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName(
						"two_phase::unsigned::tests::ocw_check_prevent_duplicate",
					),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| {
					test::assert_test_result(ocw_check_prevent_duplicate())
				}),
			};
			fn ocw_check_prevent_duplicate() {
				let (mut ext, _) = ExtBuilder::default().build_offchainify(0);
				ext . execute_with ( | | { roll_to ( 15 ) ; { match ( & TwoPhase :: current_phase ( ) , & Phase :: Unsigned ( ( true , 15 ) ) ) { ( left_val , right_val ) => { if ! ( * left_val == * right_val ) { { :: std :: rt :: begin_panic_fmt ( & :: core :: fmt :: Arguments :: new_v1 ( & [ "assertion failed: `(left == right)`\n  left: `" , "`,\n right: `" , "`" ] , & match ( & & * left_val , & & * right_val ) { ( arg0 , arg1 ) => [ :: core :: fmt :: ArgumentV1 :: new ( arg0 , :: core :: fmt :: Debug :: fmt ) , :: core :: fmt :: ArgumentV1 :: new ( arg1 , :: core :: fmt :: Debug :: fmt ) ] , } ) ) } } } } } ; if ! TwoPhase :: set_check_offchain_execution_status ( 15 ) . is_ok ( ) { { :: std :: rt :: begin_panic ( "assertion failed: TwoPhase::set_check_offchain_execution_status(15).is_ok()" ) } } ; if ! TwoPhase :: set_check_offchain_execution_status ( 16 ) . is_err ( ) { { :: std :: rt :: begin_panic ( "assertion failed: TwoPhase::set_check_offchain_execution_status(16).is_err()" ) } } ; if ! TwoPhase :: set_check_offchain_execution_status ( ( 16 + OFFCHAIN_REPEAT ) . into ( ) ) . is_ok ( ) { { :: std :: rt :: begin_panic ( "assertion failed: TwoPhase::set_check_offchain_execution_status((16 +\n                                                   OFFCHAIN_REPEAT).into()).is_ok()" ) } } ; if ! TwoPhase :: set_check_offchain_execution_status ( ( 16 + OFFCHAIN_REPEAT - 3 ) . into ( ) ) . is_err ( ) { { :: std :: rt :: begin_panic ( "assertion failed: TwoPhase::set_check_offchain_execution_status((16 + OFFCHAIN_REPEAT -\n                                                   3).into()).is_err()" ) } } ; if ! TwoPhase :: set_check_offchain_execution_status ( ( 16 + OFFCHAIN_REPEAT - 2 ) . into ( ) ) . is_err ( ) { { :: std :: rt :: begin_panic ( "assertion failed: TwoPhase::set_check_offchain_execution_status((16 + OFFCHAIN_REPEAT -\n                                                   2).into()).is_err()" ) } } ; if ! TwoPhase :: set_check_offchain_execution_status ( ( 16 + OFFCHAIN_REPEAT - 1 ) . into ( ) ) . is_err ( ) { { :: std :: rt :: begin_panic ( "assertion failed: TwoPhase::set_check_offchain_execution_status((16 + OFFCHAIN_REPEAT -\n                                                   1).into()).is_err()" ) } } ; } )
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const ocw_only_runs_when_signed_open_now: test::TestDescAndFn =
				test::TestDescAndFn {
					desc: test::TestDesc {
						name: test::StaticTestName(
							"two_phase::unsigned::tests::ocw_only_runs_when_signed_open_now",
						),
						ignore: false,
						allow_fail: false,
						should_panic: test::ShouldPanic::No,
						test_type: test::TestType::UnitTest,
					},
					testfn: test::StaticTestFn(|| {
						test::assert_test_result(ocw_only_runs_when_signed_open_now())
					}),
				};
			fn ocw_only_runs_when_signed_open_now() {
				let (mut ext, pool) = ExtBuilder::default().build_offchainify(0);
				ext.execute_with(|| {
					roll_to(15);
					{
						match (&TwoPhase::current_phase(), &Phase::Unsigned((true, 15))) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					let mut storage = StorageValueRef::persistent(&OFFCHAIN_HEAD_DB);
					TwoPhase::offchain_worker(14);
					if !pool.read().transactions.len().is_zero() {
						{
							::std::rt::begin_panic(
								"assertion failed: pool.read().transactions.len().is_zero()",
							)
						}
					};
					storage.clear();
					TwoPhase::offchain_worker(16);
					if !pool.read().transactions.len().is_zero() {
						{
							::std::rt::begin_panic(
								"assertion failed: pool.read().transactions.len().is_zero()",
							)
						}
					};
					storage.clear();
					TwoPhase::offchain_worker(15);
					if !!pool.read().transactions.len().is_zero() {
						{
							::std::rt::begin_panic(
								"assertion failed: !pool.read().transactions.len().is_zero()",
							)
						}
					};
				})
			}
			extern crate test;
			#[cfg(test)]
			#[rustc_test_marker]
			pub const ocw_can_submit_to_pool: test::TestDescAndFn = test::TestDescAndFn {
				desc: test::TestDesc {
					name: test::StaticTestName(
						"two_phase::unsigned::tests::ocw_can_submit_to_pool",
					),
					ignore: false,
					allow_fail: false,
					should_panic: test::ShouldPanic::No,
					test_type: test::TestType::UnitTest,
				},
				testfn: test::StaticTestFn(|| test::assert_test_result(ocw_can_submit_to_pool())),
			};
			fn ocw_can_submit_to_pool() {
				let (mut ext, pool) = ExtBuilder::default().build_offchainify(0);
				ext.execute_with(|| {
					roll_to(15);
					{
						match (&TwoPhase::current_phase(), &Phase::Unsigned((true, 15))) {
							(left_val, right_val) => {
								if !(*left_val == *right_val) {
									{
										::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
											&[
												"assertion failed: `(left == right)`\n  left: `",
												"`,\n right: `",
												"`",
											],
											&match (&&*left_val, &&*right_val) {
												(arg0, arg1) => [
													::core::fmt::ArgumentV1::new(
														arg0,
														::core::fmt::Debug::fmt,
													),
													::core::fmt::ArgumentV1::new(
														arg1,
														::core::fmt::Debug::fmt,
													),
												],
											},
										))
									}
								}
							}
						}
					};
					TwoPhase::offchain_worker(15);
					let encoded = pool.read().transactions[0].clone();
					let extrinsic: Extrinsic = Decode::decode(&mut &*encoded).unwrap();
					let call = extrinsic.call;
					match call {
						OuterCall::TwoPhase(Call::submit_unsigned(_)) => true,
						_ => false,
					};
				})
			}
		}
	}
	/// The compact solution type used by this crate. This is provided from the [`ElectionDataProvider`]
	/// implementer.
	pub type CompactOf<T> = <<T as Trait>::ElectionDataProvider as ElectionDataProvider<
		<T as frame_system::Trait>::AccountId,
		<T as frame_system::Trait>::BlockNumber,
	>>::CompactSolution;
	/// The voter index. Derived from [`CompactOf`].
	pub type CompactVoterIndexOf<T> = <CompactOf<T> as CompactSolution>::Voter;
	/// The target index. Derived from [`CompactOf`].
	pub type CompactTargetIndexOf<T> = <CompactOf<T> as CompactSolution>::Target;
	/// The accuracy of the election. Derived from [`CompactOf`].
	pub type CompactAccuracyOf<T> = <CompactOf<T> as CompactSolution>::VoteWeight;
	type BalanceOf<T> =
		<<T as Trait>::Currency as Currency<<T as frame_system::Trait>::AccountId>>::Balance;
	type PositiveImbalanceOf<T> = <<T as Trait>::Currency as Currency<
		<T as frame_system::Trait>::AccountId,
	>>::PositiveImbalance;
	type NegativeImbalanceOf<T> = <<T as Trait>::Currency as Currency<
		<T as frame_system::Trait>::AccountId,
	>>::NegativeImbalance;
	/// Current phase of the pallet.
	pub enum Phase<Bn> {
		/// Nothing, the election is not happening.
		Off,
		/// Signed phase is open.
		Signed,
		/// Unsigned phase is open.
		Unsigned((bool, Bn)),
	}
	impl<Bn> ::core::marker::StructuralPartialEq for Phase<Bn> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<Bn: ::core::cmp::PartialEq> ::core::cmp::PartialEq for Phase<Bn> {
		#[inline]
		fn eq(&self, other: &Phase<Bn>) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(&Phase::Unsigned(ref __self_0), &Phase::Unsigned(ref __arg_1_0)) => {
							(*__self_0) == (*__arg_1_0)
						}
						_ => true,
					}
				} else {
					false
				}
			}
		}
		#[inline]
		fn ne(&self, other: &Phase<Bn>) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(&Phase::Unsigned(ref __self_0), &Phase::Unsigned(ref __arg_1_0)) => {
							(*__self_0) != (*__arg_1_0)
						}
						_ => false,
					}
				} else {
					true
				}
			}
		}
	}
	impl<Bn> ::core::marker::StructuralEq for Phase<Bn> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<Bn: ::core::cmp::Eq> ::core::cmp::Eq for Phase<Bn> {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<(bool, Bn)>;
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<Bn: ::core::clone::Clone> ::core::clone::Clone for Phase<Bn> {
		#[inline]
		fn clone(&self) -> Phase<Bn> {
			match (&*self,) {
				(&Phase::Off,) => Phase::Off,
				(&Phase::Signed,) => Phase::Signed,
				(&Phase::Unsigned(ref __self_0),) => {
					Phase::Unsigned(::core::clone::Clone::clone(&(*__self_0)))
				}
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<Bn: ::core::marker::Copy> ::core::marker::Copy for Phase<Bn> {}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<Bn> _parity_scale_codec::Encode for Phase<Bn>
		where
			Bn: _parity_scale_codec::Encode,
			(bool, Bn): _parity_scale_codec::Encode,
		{
			fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {
				match *self {
					Phase::Off => {
						dest.push_byte(0usize as u8);
					}
					Phase::Signed => {
						dest.push_byte(1usize as u8);
					}
					Phase::Unsigned(ref aa) => {
						dest.push_byte(2usize as u8);
						dest.push(aa);
					}
					_ => (),
				}
			}
		}
		impl<Bn> _parity_scale_codec::EncodeLike for Phase<Bn>
		where
			Bn: _parity_scale_codec::Encode,
			(bool, Bn): _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<Bn> _parity_scale_codec::Decode for Phase<Bn>
		where
			Bn: _parity_scale_codec::Decode,
			(bool, Bn): _parity_scale_codec::Decode,
		{
			fn decode<DecIn: _parity_scale_codec::Input>(
				input: &mut DecIn,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				match input.read_byte()? {
					x if x == 0usize as u8 => Ok(Phase::Off),
					x if x == 1usize as u8 => Ok(Phase::Signed),
					x if x == 2usize as u8 => Ok(Phase::Unsigned({
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => return Err("Error decoding field Phase :: Unsigned.0".into()),
							Ok(a) => a,
						}
					})),
					x => Err("No such variant in enum Phase".into()),
				}
			}
		}
	};
	impl<Bn> core::fmt::Debug for Phase<Bn>
	where
		Bn: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
				Self::Off => fmt.debug_tuple("Phase::Off").finish(),
				Self::Signed => fmt.debug_tuple("Phase::Signed").finish(),
				Self::Unsigned(ref a0) => fmt.debug_tuple("Phase::Unsigned").field(a0).finish(),
				_ => Ok(()),
			}
		}
	}
	impl<Bn> Default for Phase<Bn> {
		fn default() -> Self {
			Phase::Off
		}
	}
	impl<Bn: PartialEq + Eq> Phase<Bn> {
		/// Weather the phase is signed or not.
		pub fn is_signed(&self) -> bool {
			match self {
				Phase::Signed => true,
				_ => false,
			}
		}
		/// Weather the phase is unsigned or not.
		pub fn is_unsigned(&self) -> bool {
			match self {
				Phase::Unsigned(_) => true,
				_ => false,
			}
		}
		/// Weather the phase is unsigned and open or not, with specific start.
		pub fn is_unsigned_open_at(&self, at: Bn) -> bool {
			match self {
				Phase::Unsigned((true, real)) if *real == at => true,
				_ => false,
			}
		}
		/// Weather the phase is unsigned and open or not.
		pub fn is_unsigned_open(&self) -> bool {
			match self {
				Phase::Unsigned((true, _)) => true,
				_ => false,
			}
		}
		/// Weather the phase is off or not.
		pub fn is_off(&self) -> bool {
			match self {
				Phase::Off => true,
				_ => false,
			}
		}
	}
	/// The type of `Computation` that provided this election data.
	pub enum ElectionCompute {
		/// Election was computed on-chain.
		OnChain,
		/// Election was computed with a signed submission.
		Signed,
		/// Election was computed with an unsigned submission.
		Unsigned,
	}
	impl ::core::marker::StructuralPartialEq for ElectionCompute {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::PartialEq for ElectionCompute {
		#[inline]
		fn eq(&self, other: &ElectionCompute) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						_ => true,
					}
				} else {
					false
				}
			}
		}
	}
	impl ::core::marker::StructuralEq for ElectionCompute {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::Eq for ElectionCompute {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::clone::Clone for ElectionCompute {
		#[inline]
		fn clone(&self) -> ElectionCompute {
			{
				*self
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::marker::Copy for ElectionCompute {}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Encode for ElectionCompute {
			fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {
				match *self {
					ElectionCompute::OnChain => {
						dest.push_byte(0usize as u8);
					}
					ElectionCompute::Signed => {
						dest.push_byte(1usize as u8);
					}
					ElectionCompute::Unsigned => {
						dest.push_byte(2usize as u8);
					}
					_ => (),
				}
			}
		}
		impl _parity_scale_codec::EncodeLike for ElectionCompute {}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Decode for ElectionCompute {
			fn decode<DecIn: _parity_scale_codec::Input>(
				input: &mut DecIn,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				match input.read_byte()? {
					x if x == 0usize as u8 => Ok(ElectionCompute::OnChain),
					x if x == 1usize as u8 => Ok(ElectionCompute::Signed),
					x if x == 2usize as u8 => Ok(ElectionCompute::Unsigned),
					x => Err("No such variant in enum ElectionCompute".into()),
				}
			}
		}
	};
	impl core::fmt::Debug for ElectionCompute {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
				Self::OnChain => fmt.debug_tuple("ElectionCompute::OnChain").finish(),
				Self::Signed => fmt.debug_tuple("ElectionCompute::Signed").finish(),
				Self::Unsigned => fmt.debug_tuple("ElectionCompute::Unsigned").finish(),
				_ => Ok(()),
			}
		}
	}
	impl Default for ElectionCompute {
		fn default() -> Self {
			ElectionCompute::OnChain
		}
	}
	/// A raw, unchecked solution.
	///
	/// Such a solution should never become effective in anyway before being checked by the
	/// [`Module::feasibility_check`]
	pub struct RawSolution<C> {
		/// Compact election edges.
		compact: C,
		/// The _claimed_ score of the solution.
		score: ElectionScore,
	}
	impl<C> ::core::marker::StructuralPartialEq for RawSolution<C> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<C: ::core::cmp::PartialEq> ::core::cmp::PartialEq for RawSolution<C> {
		#[inline]
		fn eq(&self, other: &RawSolution<C>) -> bool {
			match *other {
				RawSolution {
					compact: ref __self_1_0,
					score: ref __self_1_1,
				} => match *self {
					RawSolution {
						compact: ref __self_0_0,
						score: ref __self_0_1,
					} => (*__self_0_0) == (*__self_1_0) && (*__self_0_1) == (*__self_1_1),
				},
			}
		}
		#[inline]
		fn ne(&self, other: &RawSolution<C>) -> bool {
			match *other {
				RawSolution {
					compact: ref __self_1_0,
					score: ref __self_1_1,
				} => match *self {
					RawSolution {
						compact: ref __self_0_0,
						score: ref __self_0_1,
					} => (*__self_0_0) != (*__self_1_0) || (*__self_0_1) != (*__self_1_1),
				},
			}
		}
	}
	impl<C> ::core::marker::StructuralEq for RawSolution<C> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<C: ::core::cmp::Eq> ::core::cmp::Eq for RawSolution<C> {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<C>;
				let _: ::core::cmp::AssertParamIsEq<ElectionScore>;
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<C: ::core::clone::Clone> ::core::clone::Clone for RawSolution<C> {
		#[inline]
		fn clone(&self) -> RawSolution<C> {
			match *self {
				RawSolution {
					compact: ref __self_0_0,
					score: ref __self_0_1,
				} => RawSolution {
					compact: ::core::clone::Clone::clone(&(*__self_0_0)),
					score: ::core::clone::Clone::clone(&(*__self_0_1)),
				},
			}
		}
	}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<C> _parity_scale_codec::Encode for RawSolution<C>
		where
			C: _parity_scale_codec::Encode,
			C: _parity_scale_codec::Encode,
		{
			fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {
				dest.push(&self.compact);
				dest.push(&self.score);
			}
		}
		impl<C> _parity_scale_codec::EncodeLike for RawSolution<C>
		where
			C: _parity_scale_codec::Encode,
			C: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<C> _parity_scale_codec::Decode for RawSolution<C>
		where
			C: _parity_scale_codec::Decode,
			C: _parity_scale_codec::Decode,
		{
			fn decode<DecIn: _parity_scale_codec::Input>(
				input: &mut DecIn,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(RawSolution {
					compact: {
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => return Err("Error decoding field RawSolution.compact".into()),
							Ok(a) => a,
						}
					},
					score: {
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => return Err("Error decoding field RawSolution.score".into()),
							Ok(a) => a,
						}
					},
				})
			}
		}
	};
	impl<C> core::fmt::Debug for RawSolution<C>
	where
		C: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_struct("RawSolution")
				.field("compact", &self.compact)
				.field("score", &self.score)
				.finish()
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<C: ::core::default::Default> ::core::default::Default for RawSolution<C> {
		#[inline]
		fn default() -> RawSolution<C> {
			RawSolution {
				compact: ::core::default::Default::default(),
				score: ::core::default::Default::default(),
			}
		}
	}
	/// A raw, unchecked signed submission.
	///
	/// This is just a wrapper around [`RawSolution`] and some additional info.
	pub struct SignedSubmission<A, B: HasCompact, C> {
		/// Who submitted this solution.
		who: A,
		/// The deposit reserved for storing this solution.
		deposit: B,
		/// The reward that should be given to this solution, if chosen the as the final one.
		reward: B,
		/// The raw solution itself.
		solution: RawSolution<C>,
	}
	impl<A, B: HasCompact, C> ::core::marker::StructuralPartialEq for SignedSubmission<A, B, C> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<
			A: ::core::cmp::PartialEq,
			B: ::core::cmp::PartialEq + HasCompact,
			C: ::core::cmp::PartialEq,
		> ::core::cmp::PartialEq for SignedSubmission<A, B, C>
	{
		#[inline]
		fn eq(&self, other: &SignedSubmission<A, B, C>) -> bool {
			match *other {
				SignedSubmission {
					who: ref __self_1_0,
					deposit: ref __self_1_1,
					reward: ref __self_1_2,
					solution: ref __self_1_3,
				} => match *self {
					SignedSubmission {
						who: ref __self_0_0,
						deposit: ref __self_0_1,
						reward: ref __self_0_2,
						solution: ref __self_0_3,
					} => {
						(*__self_0_0) == (*__self_1_0)
							&& (*__self_0_1) == (*__self_1_1)
							&& (*__self_0_2) == (*__self_1_2)
							&& (*__self_0_3) == (*__self_1_3)
					}
				},
			}
		}
		#[inline]
		fn ne(&self, other: &SignedSubmission<A, B, C>) -> bool {
			match *other {
				SignedSubmission {
					who: ref __self_1_0,
					deposit: ref __self_1_1,
					reward: ref __self_1_2,
					solution: ref __self_1_3,
				} => match *self {
					SignedSubmission {
						who: ref __self_0_0,
						deposit: ref __self_0_1,
						reward: ref __self_0_2,
						solution: ref __self_0_3,
					} => {
						(*__self_0_0) != (*__self_1_0)
							|| (*__self_0_1) != (*__self_1_1)
							|| (*__self_0_2) != (*__self_1_2)
							|| (*__self_0_3) != (*__self_1_3)
					}
				},
			}
		}
	}
	impl<A, B: HasCompact, C> ::core::marker::StructuralEq for SignedSubmission<A, B, C> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::cmp::Eq, B: ::core::cmp::Eq + HasCompact, C: ::core::cmp::Eq> ::core::cmp::Eq
		for SignedSubmission<A, B, C>
	{
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<A>;
				let _: ::core::cmp::AssertParamIsEq<B>;
				let _: ::core::cmp::AssertParamIsEq<B>;
				let _: ::core::cmp::AssertParamIsEq<RawSolution<C>>;
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<
			A: ::core::clone::Clone,
			B: ::core::clone::Clone + HasCompact,
			C: ::core::clone::Clone,
		> ::core::clone::Clone for SignedSubmission<A, B, C>
	{
		#[inline]
		fn clone(&self) -> SignedSubmission<A, B, C> {
			match *self {
				SignedSubmission {
					who: ref __self_0_0,
					deposit: ref __self_0_1,
					reward: ref __self_0_2,
					solution: ref __self_0_3,
				} => SignedSubmission {
					who: ::core::clone::Clone::clone(&(*__self_0_0)),
					deposit: ::core::clone::Clone::clone(&(*__self_0_1)),
					reward: ::core::clone::Clone::clone(&(*__self_0_2)),
					solution: ::core::clone::Clone::clone(&(*__self_0_3)),
				},
			}
		}
	}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<A, B: HasCompact, C> _parity_scale_codec::Encode for SignedSubmission<A, B, C>
		where
			A: _parity_scale_codec::Encode,
			A: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			RawSolution<C>: _parity_scale_codec::Encode,
			RawSolution<C>: _parity_scale_codec::Encode,
		{
			fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {
				dest.push(&self.who);
				dest.push(&self.deposit);
				dest.push(&self.reward);
				dest.push(&self.solution);
			}
		}
		impl<A, B: HasCompact, C> _parity_scale_codec::EncodeLike for SignedSubmission<A, B, C>
		where
			A: _parity_scale_codec::Encode,
			A: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			B: _parity_scale_codec::Encode,
			RawSolution<C>: _parity_scale_codec::Encode,
			RawSolution<C>: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<A, B: HasCompact, C> _parity_scale_codec::Decode for SignedSubmission<A, B, C>
		where
			A: _parity_scale_codec::Decode,
			A: _parity_scale_codec::Decode,
			B: _parity_scale_codec::Decode,
			B: _parity_scale_codec::Decode,
			B: _parity_scale_codec::Decode,
			B: _parity_scale_codec::Decode,
			RawSolution<C>: _parity_scale_codec::Decode,
			RawSolution<C>: _parity_scale_codec::Decode,
		{
			fn decode<DecIn: _parity_scale_codec::Input>(
				input: &mut DecIn,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(SignedSubmission {
					who: {
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err("Error decoding field SignedSubmission.who".into())
							}
							Ok(a) => a,
						}
					},
					deposit: {
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err("Error decoding field SignedSubmission.deposit".into())
							}
							Ok(a) => a,
						}
					},
					reward: {
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err("Error decoding field SignedSubmission.reward".into())
							}
							Ok(a) => a,
						}
					},
					solution: {
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err("Error decoding field SignedSubmission.solution".into())
							}
							Ok(a) => a,
						}
					},
				})
			}
		}
	};
	impl<A, B: HasCompact, C> core::fmt::Debug for SignedSubmission<A, B, C>
	where
		A: core::fmt::Debug,
		B: core::fmt::Debug,
		C: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_struct("SignedSubmission")
				.field("who", &self.who)
				.field("deposit", &self.deposit)
				.field("reward", &self.reward)
				.field("solution", &self.solution)
				.finish()
		}
	}
	/// A checked and parsed solution, ready to be enacted.
	pub struct ReadySolution<A> {
		/// The final supports of the solution. This is target-major vector, storing each winners, total
		/// backing, and each individual backer.
		supports: Supports<A>,
		/// The score of the solution.
		///
		/// This is needed to potentially challenge the solution.
		score: ElectionScore,
		/// How this election was computed.
		compute: ElectionCompute,
	}
	impl<A> ::core::marker::StructuralPartialEq for ReadySolution<A> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::cmp::PartialEq> ::core::cmp::PartialEq for ReadySolution<A> {
		#[inline]
		fn eq(&self, other: &ReadySolution<A>) -> bool {
			match *other {
				ReadySolution {
					supports: ref __self_1_0,
					score: ref __self_1_1,
					compute: ref __self_1_2,
				} => match *self {
					ReadySolution {
						supports: ref __self_0_0,
						score: ref __self_0_1,
						compute: ref __self_0_2,
					} => {
						(*__self_0_0) == (*__self_1_0)
							&& (*__self_0_1) == (*__self_1_1)
							&& (*__self_0_2) == (*__self_1_2)
					}
				},
			}
		}
		#[inline]
		fn ne(&self, other: &ReadySolution<A>) -> bool {
			match *other {
				ReadySolution {
					supports: ref __self_1_0,
					score: ref __self_1_1,
					compute: ref __self_1_2,
				} => match *self {
					ReadySolution {
						supports: ref __self_0_0,
						score: ref __self_0_1,
						compute: ref __self_0_2,
					} => {
						(*__self_0_0) != (*__self_1_0)
							|| (*__self_0_1) != (*__self_1_1)
							|| (*__self_0_2) != (*__self_1_2)
					}
				},
			}
		}
	}
	impl<A> ::core::marker::StructuralEq for ReadySolution<A> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::cmp::Eq> ::core::cmp::Eq for ReadySolution<A> {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<Supports<A>>;
				let _: ::core::cmp::AssertParamIsEq<ElectionScore>;
				let _: ::core::cmp::AssertParamIsEq<ElectionCompute>;
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::clone::Clone> ::core::clone::Clone for ReadySolution<A> {
		#[inline]
		fn clone(&self) -> ReadySolution<A> {
			match *self {
				ReadySolution {
					supports: ref __self_0_0,
					score: ref __self_0_1,
					compute: ref __self_0_2,
				} => ReadySolution {
					supports: ::core::clone::Clone::clone(&(*__self_0_0)),
					score: ::core::clone::Clone::clone(&(*__self_0_1)),
					compute: ::core::clone::Clone::clone(&(*__self_0_2)),
				},
			}
		}
	}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<A> _parity_scale_codec::Encode for ReadySolution<A>
		where
			Supports<A>: _parity_scale_codec::Encode,
			Supports<A>: _parity_scale_codec::Encode,
		{
			fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {
				dest.push(&self.supports);
				dest.push(&self.score);
				dest.push(&self.compute);
			}
		}
		impl<A> _parity_scale_codec::EncodeLike for ReadySolution<A>
		where
			Supports<A>: _parity_scale_codec::Encode,
			Supports<A>: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<A> _parity_scale_codec::Decode for ReadySolution<A>
		where
			Supports<A>: _parity_scale_codec::Decode,
			Supports<A>: _parity_scale_codec::Decode,
		{
			fn decode<DecIn: _parity_scale_codec::Input>(
				input: &mut DecIn,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(ReadySolution {
					supports: {
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err("Error decoding field ReadySolution.supports".into())
							}
							Ok(a) => a,
						}
					},
					score: {
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => return Err("Error decoding field ReadySolution.score".into()),
							Ok(a) => a,
						}
					},
					compute: {
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err("Error decoding field ReadySolution.compute".into())
							}
							Ok(a) => a,
						}
					},
				})
			}
		}
	};
	impl<A> core::fmt::Debug for ReadySolution<A>
	where
		A: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_struct("ReadySolution")
				.field("supports", &self.supports)
				.field("score", &self.score)
				.field("compute", &self.compute)
				.finish()
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<A: ::core::default::Default> ::core::default::Default for ReadySolution<A> {
		#[inline]
		fn default() -> ReadySolution<A> {
			ReadySolution {
				supports: ::core::default::Default::default(),
				score: ::core::default::Default::default(),
				compute: ::core::default::Default::default(),
			}
		}
	}
	/// Witness data about the size of the election.
	///
	/// This is needed for proper weight calculation.
	pub struct WitnessData {
		/// Number of all voters.
		///
		/// This must match the on-chain snapshot.
		#[codec(compact)]
		voters: u32,
		/// Number of all targets.
		///
		/// This must match the on-chain snapshot.
		#[codec(compact)]
		targets: u32,
	}
	impl ::core::marker::StructuralPartialEq for WitnessData {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::PartialEq for WitnessData {
		#[inline]
		fn eq(&self, other: &WitnessData) -> bool {
			match *other {
				WitnessData {
					voters: ref __self_1_0,
					targets: ref __self_1_1,
				} => match *self {
					WitnessData {
						voters: ref __self_0_0,
						targets: ref __self_0_1,
					} => (*__self_0_0) == (*__self_1_0) && (*__self_0_1) == (*__self_1_1),
				},
			}
		}
		#[inline]
		fn ne(&self, other: &WitnessData) -> bool {
			match *other {
				WitnessData {
					voters: ref __self_1_0,
					targets: ref __self_1_1,
				} => match *self {
					WitnessData {
						voters: ref __self_0_0,
						targets: ref __self_0_1,
					} => (*__self_0_0) != (*__self_1_0) || (*__self_0_1) != (*__self_1_1),
				},
			}
		}
	}
	impl ::core::marker::StructuralEq for WitnessData {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::Eq for WitnessData {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<u32>;
				let _: ::core::cmp::AssertParamIsEq<u32>;
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::clone::Clone for WitnessData {
		#[inline]
		fn clone(&self) -> WitnessData {
			{
				let _: ::core::clone::AssertParamIsClone<u32>;
				let _: ::core::clone::AssertParamIsClone<u32>;
				*self
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::marker::Copy for WitnessData {}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Encode for WitnessData {
			fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {
				{
					dest . push ( & < < u32 as _parity_scale_codec :: HasCompact > :: Type as _parity_scale_codec :: EncodeAsRef < '_ , u32 > > :: from ( & self . voters ) ) ;
				}
				{
					dest . push ( & < < u32 as _parity_scale_codec :: HasCompact > :: Type as _parity_scale_codec :: EncodeAsRef < '_ , u32 > > :: from ( & self . targets ) ) ;
				}
			}
		}
		impl _parity_scale_codec::EncodeLike for WitnessData {}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Decode for WitnessData {
			fn decode<DecIn: _parity_scale_codec::Input>(
				input: &mut DecIn,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(WitnessData {
					voters: {
						let res = < < u32 as _parity_scale_codec :: HasCompact > :: Type as _parity_scale_codec :: Decode > :: decode ( input ) ;
						match res {
							Err(_) => return Err("Error decoding field WitnessData.voters".into()),
							Ok(a) => a.into(),
						}
					},
					targets: {
						let res = < < u32 as _parity_scale_codec :: HasCompact > :: Type as _parity_scale_codec :: Decode > :: decode ( input ) ;
						match res {
							Err(_) => return Err("Error decoding field WitnessData.targets".into()),
							Ok(a) => a.into(),
						}
					},
				})
			}
		}
	};
	impl core::fmt::Debug for WitnessData {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_struct("WitnessData")
				.field("voters", &self.voters)
				.field("targets", &self.targets)
				.finish()
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::default::Default for WitnessData {
		#[inline]
		fn default() -> WitnessData {
			WitnessData {
				voters: ::core::default::Default::default(),
				targets: ::core::default::Default::default(),
			}
		}
	}
	/// The crate errors. Note that this is different from the [`PalletError`].
	pub enum Error {
		/// A feasibility error.
		Feasibility(FeasibilityError),
		/// An error in the on-chain fallback.
		OnChainFallback(crate::onchain::Error),
		/// An internal error in the NPoS elections crate.
		NposElections(sp_npos_elections::Error),
		/// Snapshot data was unavailable unexpectedly.
		SnapshotUnAvailable,
		/// Submitting a transaction to the pool failed.
		///
		/// This can only happen in the unsigned phase.
		PoolSubmissionFailed,
	}
	impl core::fmt::Debug for Error {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
				Self::Feasibility(ref a0) => {
					fmt.debug_tuple("Error::Feasibility").field(a0).finish()
				}
				Self::OnChainFallback(ref a0) => {
					fmt.debug_tuple("Error::OnChainFallback").field(a0).finish()
				}
				Self::NposElections(ref a0) => {
					fmt.debug_tuple("Error::NposElections").field(a0).finish()
				}
				Self::SnapshotUnAvailable => fmt.debug_tuple("Error::SnapshotUnAvailable").finish(),
				Self::PoolSubmissionFailed => {
					fmt.debug_tuple("Error::PoolSubmissionFailed").finish()
				}
				_ => Ok(()),
			}
		}
	}
	impl ::core::marker::StructuralEq for Error {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::Eq for Error {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<FeasibilityError>;
				let _: ::core::cmp::AssertParamIsEq<crate::onchain::Error>;
				let _: ::core::cmp::AssertParamIsEq<sp_npos_elections::Error>;
			}
		}
	}
	impl ::core::marker::StructuralPartialEq for Error {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::PartialEq for Error {
		#[inline]
		fn eq(&self, other: &Error) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(&Error::Feasibility(ref __self_0), &Error::Feasibility(ref __arg_1_0)) => {
							(*__self_0) == (*__arg_1_0)
						}
						(
							&Error::OnChainFallback(ref __self_0),
							&Error::OnChainFallback(ref __arg_1_0),
						) => (*__self_0) == (*__arg_1_0),
						(
							&Error::NposElections(ref __self_0),
							&Error::NposElections(ref __arg_1_0),
						) => (*__self_0) == (*__arg_1_0),
						_ => true,
					}
				} else {
					false
				}
			}
		}
		#[inline]
		fn ne(&self, other: &Error) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(&Error::Feasibility(ref __self_0), &Error::Feasibility(ref __arg_1_0)) => {
							(*__self_0) != (*__arg_1_0)
						}
						(
							&Error::OnChainFallback(ref __self_0),
							&Error::OnChainFallback(ref __arg_1_0),
						) => (*__self_0) != (*__arg_1_0),
						(
							&Error::NposElections(ref __self_0),
							&Error::NposElections(ref __arg_1_0),
						) => (*__self_0) != (*__arg_1_0),
						_ => false,
					}
				} else {
					true
				}
			}
		}
	}
	impl From<crate::onchain::Error> for Error {
		fn from(e: crate::onchain::Error) -> Self {
			Error::OnChainFallback(e)
		}
	}
	impl From<sp_npos_elections::Error> for Error {
		fn from(e: sp_npos_elections::Error) -> Self {
			Error::NposElections(e)
		}
	}
	/// Errors that can happen in the feasibility check.
	pub enum FeasibilityError {
		/// Wrong number of winners presented.
		WrongWinnerCount,
		/// The snapshot is not available.
		///
		/// This must be an internal error of the chain.
		SnapshotUnavailable,
		/// Internal error from the election crate.
		NposElectionError(sp_npos_elections::Error),
		/// A vote is invalid.
		InvalidVote,
		/// A voter is invalid.
		InvalidVoter,
		/// A winner is invalid.
		InvalidWinner,
		/// The given score was invalid.
		InvalidScore,
	}
	impl core::fmt::Debug for FeasibilityError {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
				Self::WrongWinnerCount => fmt
					.debug_tuple("FeasibilityError::WrongWinnerCount")
					.finish(),
				Self::SnapshotUnavailable => fmt
					.debug_tuple("FeasibilityError::SnapshotUnavailable")
					.finish(),
				Self::NposElectionError(ref a0) => fmt
					.debug_tuple("FeasibilityError::NposElectionError")
					.field(a0)
					.finish(),
				Self::InvalidVote => fmt.debug_tuple("FeasibilityError::InvalidVote").finish(),
				Self::InvalidVoter => fmt.debug_tuple("FeasibilityError::InvalidVoter").finish(),
				Self::InvalidWinner => fmt.debug_tuple("FeasibilityError::InvalidWinner").finish(),
				Self::InvalidScore => fmt.debug_tuple("FeasibilityError::InvalidScore").finish(),
				_ => Ok(()),
			}
		}
	}
	impl ::core::marker::StructuralEq for FeasibilityError {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::Eq for FeasibilityError {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<sp_npos_elections::Error>;
			}
		}
	}
	impl ::core::marker::StructuralPartialEq for FeasibilityError {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::PartialEq for FeasibilityError {
		#[inline]
		fn eq(&self, other: &FeasibilityError) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(
							&FeasibilityError::NposElectionError(ref __self_0),
							&FeasibilityError::NposElectionError(ref __arg_1_0),
						) => (*__self_0) == (*__arg_1_0),
						_ => true,
					}
				} else {
					false
				}
			}
		}
		#[inline]
		fn ne(&self, other: &FeasibilityError) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(
							&FeasibilityError::NposElectionError(ref __self_0),
							&FeasibilityError::NposElectionError(ref __arg_1_0),
						) => (*__self_0) != (*__arg_1_0),
						_ => false,
					}
				} else {
					true
				}
			}
		}
	}
	impl From<sp_npos_elections::Error> for FeasibilityError {
		fn from(e: sp_npos_elections::Error) -> Self {
			FeasibilityError::NposElectionError(e)
		}
	}
	/// The weights for this pallet.
	pub trait WeightInfo {
		fn feasibility_check() -> Weight;
		fn submit() -> Weight;
		fn submit_unsigned() -> Weight;
	}
	impl WeightInfo for () {
		fn feasibility_check() -> Weight {
			Default::default()
		}
		fn submit() -> Weight {
			Default::default()
		}
		fn submit_unsigned() -> Weight {
			Default::default()
		}
	}
	pub trait Trait: frame_system::Trait + SendTransactionTypes<Call<Self>>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<Self>>>,
	{
		/// Event type.
		type Event: From<Event<Self>> + Into<<Self as frame_system::Trait>::Event>;
		/// Currency type.
		type Currency: ReservableCurrency<Self::AccountId> + Currency<Self::AccountId>;
		/// Duration of the signed phase.
		type SignedPhase: Get<Self::BlockNumber>;
		/// Duration of the unsigned phase.
		type UnsignedPhase: Get<Self::BlockNumber>;
		/// Maximum number of singed submissions that can be queued.
		type MaxSignedSubmissions: Get<u32>;
		type SignedRewardBase: Get<BalanceOf<Self>>;
		type SignedRewardFactor: Get<Perbill>;
		type SignedRewardMax: Get<Option<BalanceOf<Self>>>;
		type SignedDepositBase: Get<BalanceOf<Self>>;
		type SignedDepositByte: Get<BalanceOf<Self>>;
		type SignedDepositWeight: Get<BalanceOf<Self>>;
		/// The minimum amount of improvement to the solution score that defines a solution as "better".
		type SolutionImprovementThreshold: Get<Perbill>;
		type UnsignedMaxIterations: Get<u32>;
		type UnsignedPriority: Get<TransactionPriority>;
		/// Handler for the slashed deposits.
		type SlashHandler: OnUnbalanced<NegativeImbalanceOf<Self>>;
		/// Handler for the rewards.
		type RewardHandler: OnUnbalanced<PositiveImbalanceOf<Self>>;
		/// Something that will provide the election data.
		type ElectionDataProvider: ElectionDataProvider<Self::AccountId, Self::BlockNumber>;
		/// The weight of the pallet.
		type WeightInfo: WeightInfo;
	}
	use self::sp_api_hidden_includes_decl_storage::hidden_include::{
		IterableStorageDoubleMap as _, IterableStorageMap as _, StorageDoubleMap as _,
		StorageMap as _, StoragePrefixedMap as _, StorageValue as _,
	};
	#[doc(hidden)]
	mod sp_api_hidden_includes_decl_storage {
		pub extern crate frame_support as hidden_include;
	}
	trait Store {
		type Round;
		type CurrentPhase;
		type SignedSubmissions;
		type QueuedSolution;
		type SnapshotTargets;
		type SnapshotVoters;
		type DesiredTargets;
	}
	impl<T: Trait + 'static> Store for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Round = Round;
		type CurrentPhase = CurrentPhase<T>;
		type SignedSubmissions = SignedSubmissions<T>;
		type QueuedSolution = QueuedSolution<T>;
		type SnapshotTargets = SnapshotTargets<T>;
		type SnapshotVoters = SnapshotVoters<T>;
		type DesiredTargets = DesiredTargets;
	}
	impl<T: Trait + 'static> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		/// Internal counter ofr the number of rounds.
		///
		/// This is useful for de-duplication of transactions submitted to the pool, and general
		/// diagnostics of the module.
		///
		/// This is merely incremented once per every time that signed phase starts.
		pub fn round() -> u32 {
			< Round < > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < u32 > > :: get ( )
		}
		/// Current phase.
		pub fn current_phase() -> Phase<T::BlockNumber> {
			< CurrentPhase < T > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < Phase < T :: BlockNumber > > > :: get ( )
		}
		/// Sorted (worse -> best) list of unchecked, signed solutions.
		pub fn signed_submissions(
		) -> Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>> {
			< SignedSubmissions < T > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < Vec < SignedSubmission < T :: AccountId , BalanceOf < T > , CompactOf < T > > > > > :: get ( )
		}
		/// Current best solution, signed or unsigned.
		pub fn queued_solution() -> Option<ReadySolution<T::AccountId>> {
			< QueuedSolution < T > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < ReadySolution < T :: AccountId > > > :: get ( )
		}
		/// Snapshot of all Voters.
		///
		/// This is created at the beginning of the signed phase and cleared upon calling `elect`.
		pub fn snapshot_targets() -> Option<Vec<T::AccountId>> {
			< SnapshotTargets < T > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < Vec < T :: AccountId > > > :: get ( )
		}
		/// Snapshot of all targets.
		///
		/// This is created at the beginning of the signed phase and cleared upon calling `elect`.
		pub fn snapshot_voters() -> Option<Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>> {
			< SnapshotVoters < T > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < Vec < ( T :: AccountId , VoteWeight , Vec < T :: AccountId > ) > > > :: get ( )
		}
		/// Desired number of targets to elect.
		///
		/// This is created at the beginning of the signed phase and cleared upon calling `elect`.
		pub fn desired_targets() -> u32 {
			< DesiredTargets < > as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: storage :: StorageValue < u32 > > :: get ( )
		}
	}
	#[doc(hidden)]
	pub struct __GetByteStructRound<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_Round:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Trait> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructRound<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_Round
				.get_or_init(|| {
					let def_val: u32 = 0;
					<u32 as Encode>::encode(&def_val)
				})
				.clone()
		}
	}
	unsafe impl<T: Trait> Send for __GetByteStructRound<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Trait> Sync for __GetByteStructRound<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[doc(hidden)]
	pub struct __GetByteStructCurrentPhase<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_CurrentPhase:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Trait> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructCurrentPhase<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_CurrentPhase
				.get_or_init(|| {
					let def_val: Phase<T::BlockNumber> = Phase::Off;
					<Phase<T::BlockNumber> as Encode>::encode(&def_val)
				})
				.clone()
		}
	}
	unsafe impl<T: Trait> Send for __GetByteStructCurrentPhase<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Trait> Sync for __GetByteStructCurrentPhase<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[doc(hidden)]
	pub struct __GetByteStructSignedSubmissions<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_SignedSubmissions:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Trait> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructSignedSubmissions<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_SignedSubmissions . get_or_init ( | | { let def_val : Vec < SignedSubmission < T :: AccountId , BalanceOf < T > , CompactOf < T > > > = Default :: default ( ) ; < Vec < SignedSubmission < T :: AccountId , BalanceOf < T > , CompactOf < T > > > as Encode > :: encode ( & def_val ) } ) . clone ( )
		}
	}
	unsafe impl<T: Trait> Send for __GetByteStructSignedSubmissions<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Trait> Sync for __GetByteStructSignedSubmissions<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[doc(hidden)]
	pub struct __GetByteStructQueuedSolution<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_QueuedSolution:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Trait> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructQueuedSolution<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_QueuedSolution
				.get_or_init(|| {
					let def_val: Option<ReadySolution<T::AccountId>> = Default::default();
					<Option<ReadySolution<T::AccountId>> as Encode>::encode(&def_val)
				})
				.clone()
		}
	}
	unsafe impl<T: Trait> Send for __GetByteStructQueuedSolution<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Trait> Sync for __GetByteStructQueuedSolution<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[doc(hidden)]
	pub struct __GetByteStructSnapshotTargets<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_SnapshotTargets:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Trait> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructSnapshotTargets<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_SnapshotTargets
				.get_or_init(|| {
					let def_val: Option<Vec<T::AccountId>> = Default::default();
					<Option<Vec<T::AccountId>> as Encode>::encode(&def_val)
				})
				.clone()
		}
	}
	unsafe impl<T: Trait> Send for __GetByteStructSnapshotTargets<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Trait> Sync for __GetByteStructSnapshotTargets<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[doc(hidden)]
	pub struct __GetByteStructSnapshotVoters<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_SnapshotVoters:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Trait> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructSnapshotVoters<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_SnapshotVoters
				.get_or_init(|| {
					let def_val: Option<Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>> =
						Default::default();
					<Option<Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>> as Encode>::encode(
						&def_val,
					)
				})
				.clone()
		}
	}
	unsafe impl<T: Trait> Send for __GetByteStructSnapshotVoters<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Trait> Sync for __GetByteStructSnapshotVoters<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[doc(hidden)]
	pub struct __GetByteStructDesiredTargets<T>(
		pub  self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
			(T),
		>,
	);
	#[cfg(feature = "std")]
	#[allow(non_upper_case_globals)]
	static __CACHE_GET_BYTE_STRUCT_DesiredTargets:
		self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell<
			self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8>,
		> = self::sp_api_hidden_includes_decl_storage::hidden_include::once_cell::sync::OnceCell::new();
	#[cfg(feature = "std")]
	impl<T: Trait> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::DefaultByte
		for __GetByteStructDesiredTargets<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn default_byte(
			&self,
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::vec::Vec<u8> {
			use self::sp_api_hidden_includes_decl_storage::hidden_include::codec::Encode;
			__CACHE_GET_BYTE_STRUCT_DesiredTargets
				.get_or_init(|| {
					let def_val: u32 = Default::default();
					<u32 as Encode>::encode(&def_val)
				})
				.clone()
		}
	}
	unsafe impl<T: Trait> Send for __GetByteStructDesiredTargets<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	unsafe impl<T: Trait> Sync for __GetByteStructDesiredTargets<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	impl<T: Trait + 'static> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[doc(hidden)]
		pub fn storage_metadata(
		) -> self::sp_api_hidden_includes_decl_storage::hidden_include::metadata::StorageMetadata {
			self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageMetadata { prefix : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "TwoPhaseElectionProvider" ) , entries : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "Round" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Default , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "u32" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructRound :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Internal counter ofr the number of rounds." , "" , " This is useful for de-duplication of transactions submitted to the pool, and general" , " diagnostics of the module." , "" , " This is merely incremented once per every time that signed phase starts." ] ) , } , self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "CurrentPhase" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Default , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "Phase<T::BlockNumber>" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructCurrentPhase :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Current phase." ] ) , } , self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "SignedSubmissions" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Default , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructSignedSubmissions :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Sorted (worse -> best) list of unchecked, signed solutions." ] ) , } , self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "QueuedSolution" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Optional , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "ReadySolution<T::AccountId>" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructQueuedSolution :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Current best solution, signed or unsigned." ] ) , } , self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "SnapshotTargets" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Optional , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "Vec<T::AccountId>" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructSnapshotTargets :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Snapshot of all Voters." , "" , " This is created at the beginning of the signed phase and cleared upon calling `elect`." ] ) , } , self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "SnapshotVoters" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Optional , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructSnapshotVoters :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Snapshot of all targets." , "" , " This is created at the beginning of the signed phase and cleared upon calling `elect`." ] ) , } , self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryMetadata { name : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "DesiredTargets" ) , modifier : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryModifier :: Default , ty : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: StorageEntryType :: Plain ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( "u32" ) ) , default : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DefaultByteGetter ( & __GetByteStructDesiredTargets :: < T > ( self :: sp_api_hidden_includes_decl_storage :: hidden_include :: sp_std :: marker :: PhantomData ) ) ) , documentation : self :: sp_api_hidden_includes_decl_storage :: hidden_include :: metadata :: DecodeDifferent :: Encode ( & [ " Desired number of targets to elect." , "" , " This is created at the beginning of the signed phase and cleared upon calling `elect`." ] ) , } ] [ .. ] ) , }
		}
	}
	/// Hidden instance generated to be internally used when module is used without
	/// instance.
	#[doc(hidden)]
	pub struct __InherentHiddenInstance;
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::clone::Clone for __InherentHiddenInstance {
		#[inline]
		fn clone(&self) -> __InherentHiddenInstance {
			match *self {
				__InherentHiddenInstance => __InherentHiddenInstance,
			}
		}
	}
	impl ::core::marker::StructuralEq for __InherentHiddenInstance {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::Eq for __InherentHiddenInstance {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{}
		}
	}
	impl ::core::marker::StructuralPartialEq for __InherentHiddenInstance {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl ::core::cmp::PartialEq for __InherentHiddenInstance {
		#[inline]
		fn eq(&self, other: &__InherentHiddenInstance) -> bool {
			match *other {
				__InherentHiddenInstance => match *self {
					__InherentHiddenInstance => true,
				},
			}
		}
	}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Encode for __InherentHiddenInstance {
			fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {}
		}
		impl _parity_scale_codec::EncodeLike for __InherentHiddenInstance {}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl _parity_scale_codec::Decode for __InherentHiddenInstance {
			fn decode<DecIn: _parity_scale_codec::Input>(
				input: &mut DecIn,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				Ok(__InherentHiddenInstance)
			}
		}
	};
	impl core::fmt::Debug for __InherentHiddenInstance {
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_tuple("__InherentHiddenInstance").finish()
		}
	}
	impl self::sp_api_hidden_includes_decl_storage::hidden_include::traits::Instance
		for __InherentHiddenInstance
	{
		const PREFIX: &'static str = "TwoPhaseElectionProvider";
	}
	/// Internal counter ofr the number of rounds.
	///
	/// This is useful for de-duplication of transactions submitted to the pool, and general
	/// diagnostics of the module.
	///
	/// This is merely incremented once per every time that signed phase starts.
	pub struct Round(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<()>,
	);
	impl
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			u32,
		> for Round
	{
		type Query = u32;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"Round"
		}
		fn from_optional_value_to_query(v: Option<u32>) -> Self::Query {
			v.unwrap_or_else(|| 0)
		}
		fn from_query_to_optional_value(v: Self::Query) -> Option<u32> {
			Some(v)
		}
	}
	/// Current phase.
	pub struct CurrentPhase<T: Trait>(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
				(T,),
			>,
	)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	impl<T: Trait>
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			Phase<T::BlockNumber>,
		> for CurrentPhase<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Query = Phase<T::BlockNumber>;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"CurrentPhase"
		}
		fn from_optional_value_to_query(v: Option<Phase<T::BlockNumber>>) -> Self::Query {
			v.unwrap_or_else(|| Phase::Off)
		}
		fn from_query_to_optional_value(v: Self::Query) -> Option<Phase<T::BlockNumber>> {
			Some(v)
		}
	}
	/// Sorted (worse -> best) list of unchecked, signed solutions.
	pub struct SignedSubmissions<T: Trait>(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
				(T,),
			>,
	)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	impl<T: Trait>
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>,
		> for SignedSubmissions<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Query = Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"SignedSubmissions"
		}
		fn from_optional_value_to_query(
			v: Option<Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>>,
		) -> Self::Query {
			v.unwrap_or_else(|| Default::default())
		}
		fn from_query_to_optional_value(
			v: Self::Query,
		) -> Option<Vec<SignedSubmission<T::AccountId, BalanceOf<T>, CompactOf<T>>>> {
			Some(v)
		}
	}
	/// Current best solution, signed or unsigned.
	pub struct QueuedSolution<T: Trait>(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
				(T,),
			>,
	)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	impl<T: Trait>
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			ReadySolution<T::AccountId>,
		> for QueuedSolution<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Query = Option<ReadySolution<T::AccountId>>;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"QueuedSolution"
		}
		fn from_optional_value_to_query(v: Option<ReadySolution<T::AccountId>>) -> Self::Query {
			v.or_else(|| Default::default())
		}
		fn from_query_to_optional_value(v: Self::Query) -> Option<ReadySolution<T::AccountId>> {
			v
		}
	}
	/// Snapshot of all Voters.
	///
	/// This is created at the beginning of the signed phase and cleared upon calling `elect`.
	pub struct SnapshotTargets<T: Trait>(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
				(T,),
			>,
	)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	impl<T: Trait>
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			Vec<T::AccountId>,
		> for SnapshotTargets<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Query = Option<Vec<T::AccountId>>;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"SnapshotTargets"
		}
		fn from_optional_value_to_query(v: Option<Vec<T::AccountId>>) -> Self::Query {
			v.or_else(|| Default::default())
		}
		fn from_query_to_optional_value(v: Self::Query) -> Option<Vec<T::AccountId>> {
			v
		}
	}
	/// Snapshot of all targets.
	///
	/// This is created at the beginning of the signed phase and cleared upon calling `elect`.
	pub struct SnapshotVoters<T: Trait>(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<
				(T,),
			>,
	)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	impl<T: Trait>
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>,
		> for SnapshotVoters<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Query = Option<Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>>;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"SnapshotVoters"
		}
		fn from_optional_value_to_query(
			v: Option<Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>>,
		) -> Self::Query {
			v.or_else(|| Default::default())
		}
		fn from_query_to_optional_value(
			v: Self::Query,
		) -> Option<Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>> {
			v
		}
	}
	/// Desired number of targets to elect.
	///
	/// This is created at the beginning of the signed phase and cleared upon calling `elect`.
	pub struct DesiredTargets(
		self::sp_api_hidden_includes_decl_storage::hidden_include::sp_std::marker::PhantomData<()>,
	);
	impl
		self::sp_api_hidden_includes_decl_storage::hidden_include::storage::generator::StorageValue<
			u32,
		> for DesiredTargets
	{
		type Query = u32;
		fn module_prefix() -> &'static [u8] {
			< __InherentHiddenInstance as self :: sp_api_hidden_includes_decl_storage :: hidden_include :: traits :: Instance > :: PREFIX . as_bytes ( )
		}
		fn storage_prefix() -> &'static [u8] {
			b"DesiredTargets"
		}
		fn from_optional_value_to_query(v: Option<u32>) -> Self::Query {
			v.unwrap_or_else(|| Default::default())
		}
		fn from_query_to_optional_value(v: Self::Query) -> Option<u32> {
			Some(v)
		}
	}
	/// [`RawEvent`] specialized for the configuration [`Trait`]
	///
	/// [`RawEvent`]: enum.RawEvent.html
	/// [`Trait`]: trait.Trait.html
	pub type Event<T> = RawEvent<<T as frame_system::Trait>::AccountId>;
	/// Events for this module.
	///
	pub enum RawEvent<AccountId> {
		/// A solution was stored with the given compute.
		///
		/// If the solution is signed, this means that it hasn't yet been processed. If the solution
		/// is unsigned, this means that it has also been processed.
		SolutionStored(ElectionCompute),
		/// The election has been finalized, with `Some` of the given computation, or else if the
		/// election failed, `None`.
		ElectionFinalized(Option<ElectionCompute>),
		/// An account has been rewarded for their signed submission being finalized.
		Rewarded(AccountId),
		/// An account has been slashed for submitting an invalid signed submission.
		Slashed(AccountId),
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<AccountId: ::core::clone::Clone> ::core::clone::Clone for RawEvent<AccountId> {
		#[inline]
		fn clone(&self) -> RawEvent<AccountId> {
			match (&*self,) {
				(&RawEvent::SolutionStored(ref __self_0),) => {
					RawEvent::SolutionStored(::core::clone::Clone::clone(&(*__self_0)))
				}
				(&RawEvent::ElectionFinalized(ref __self_0),) => {
					RawEvent::ElectionFinalized(::core::clone::Clone::clone(&(*__self_0)))
				}
				(&RawEvent::Rewarded(ref __self_0),) => {
					RawEvent::Rewarded(::core::clone::Clone::clone(&(*__self_0)))
				}
				(&RawEvent::Slashed(ref __self_0),) => {
					RawEvent::Slashed(::core::clone::Clone::clone(&(*__self_0)))
				}
			}
		}
	}
	impl<AccountId> ::core::marker::StructuralPartialEq for RawEvent<AccountId> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<AccountId: ::core::cmp::PartialEq> ::core::cmp::PartialEq for RawEvent<AccountId> {
		#[inline]
		fn eq(&self, other: &RawEvent<AccountId>) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(
							&RawEvent::SolutionStored(ref __self_0),
							&RawEvent::SolutionStored(ref __arg_1_0),
						) => (*__self_0) == (*__arg_1_0),
						(
							&RawEvent::ElectionFinalized(ref __self_0),
							&RawEvent::ElectionFinalized(ref __arg_1_0),
						) => (*__self_0) == (*__arg_1_0),
						(&RawEvent::Rewarded(ref __self_0), &RawEvent::Rewarded(ref __arg_1_0)) => {
							(*__self_0) == (*__arg_1_0)
						}
						(&RawEvent::Slashed(ref __self_0), &RawEvent::Slashed(ref __arg_1_0)) => {
							(*__self_0) == (*__arg_1_0)
						}
						_ => unsafe { ::core::intrinsics::unreachable() },
					}
				} else {
					false
				}
			}
		}
		#[inline]
		fn ne(&self, other: &RawEvent<AccountId>) -> bool {
			{
				let __self_vi = unsafe { ::core::intrinsics::discriminant_value(&*self) };
				let __arg_1_vi = unsafe { ::core::intrinsics::discriminant_value(&*other) };
				if true && __self_vi == __arg_1_vi {
					match (&*self, &*other) {
						(
							&RawEvent::SolutionStored(ref __self_0),
							&RawEvent::SolutionStored(ref __arg_1_0),
						) => (*__self_0) != (*__arg_1_0),
						(
							&RawEvent::ElectionFinalized(ref __self_0),
							&RawEvent::ElectionFinalized(ref __arg_1_0),
						) => (*__self_0) != (*__arg_1_0),
						(&RawEvent::Rewarded(ref __self_0), &RawEvent::Rewarded(ref __arg_1_0)) => {
							(*__self_0) != (*__arg_1_0)
						}
						(&RawEvent::Slashed(ref __self_0), &RawEvent::Slashed(ref __arg_1_0)) => {
							(*__self_0) != (*__arg_1_0)
						}
						_ => unsafe { ::core::intrinsics::unreachable() },
					}
				} else {
					true
				}
			}
		}
	}
	impl<AccountId> ::core::marker::StructuralEq for RawEvent<AccountId> {}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<AccountId: ::core::cmp::Eq> ::core::cmp::Eq for RawEvent<AccountId> {
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<ElectionCompute>;
				let _: ::core::cmp::AssertParamIsEq<Option<ElectionCompute>>;
				let _: ::core::cmp::AssertParamIsEq<AccountId>;
				let _: ::core::cmp::AssertParamIsEq<AccountId>;
			}
		}
	}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<AccountId> _parity_scale_codec::Encode for RawEvent<AccountId>
		where
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
		{
			fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {
				match *self {
					RawEvent::SolutionStored(ref aa) => {
						dest.push_byte(0usize as u8);
						dest.push(aa);
					}
					RawEvent::ElectionFinalized(ref aa) => {
						dest.push_byte(1usize as u8);
						dest.push(aa);
					}
					RawEvent::Rewarded(ref aa) => {
						dest.push_byte(2usize as u8);
						dest.push(aa);
					}
					RawEvent::Slashed(ref aa) => {
						dest.push_byte(3usize as u8);
						dest.push(aa);
					}
					_ => (),
				}
			}
		}
		impl<AccountId> _parity_scale_codec::EncodeLike for RawEvent<AccountId>
		where
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
			AccountId: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<AccountId> _parity_scale_codec::Decode for RawEvent<AccountId>
		where
			AccountId: _parity_scale_codec::Decode,
			AccountId: _parity_scale_codec::Decode,
			AccountId: _parity_scale_codec::Decode,
			AccountId: _parity_scale_codec::Decode,
		{
			fn decode<DecIn: _parity_scale_codec::Input>(
				input: &mut DecIn,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				match input.read_byte()? {
					x if x == 0usize as u8 => Ok(RawEvent::SolutionStored({
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err(
									"Error decoding field RawEvent :: SolutionStored.0".into()
								)
							}
							Ok(a) => a,
						}
					})),
					x if x == 1usize as u8 => Ok(RawEvent::ElectionFinalized({
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err(
									"Error decoding field RawEvent :: ElectionFinalized.0".into()
								)
							}
							Ok(a) => a,
						}
					})),
					x if x == 2usize as u8 => Ok(RawEvent::Rewarded({
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err("Error decoding field RawEvent :: Rewarded.0".into())
							}
							Ok(a) => a,
						}
					})),
					x if x == 3usize as u8 => Ok(RawEvent::Slashed({
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err("Error decoding field RawEvent :: Slashed.0".into())
							}
							Ok(a) => a,
						}
					})),
					x => Err("No such variant in enum RawEvent".into()),
				}
			}
		}
	};
	impl<AccountId> core::fmt::Debug for RawEvent<AccountId>
	where
		AccountId: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			match self {
				Self::SolutionStored(ref a0) => fmt
					.debug_tuple("RawEvent::SolutionStored")
					.field(a0)
					.finish(),
				Self::ElectionFinalized(ref a0) => fmt
					.debug_tuple("RawEvent::ElectionFinalized")
					.field(a0)
					.finish(),
				Self::Rewarded(ref a0) => fmt.debug_tuple("RawEvent::Rewarded").field(a0).finish(),
				Self::Slashed(ref a0) => fmt.debug_tuple("RawEvent::Slashed").field(a0).finish(),
				_ => Ok(()),
			}
		}
	}
	impl<AccountId> From<RawEvent<AccountId>> for () {
		fn from(_: RawEvent<AccountId>) -> () {
			()
		}
	}
	impl<AccountId> RawEvent<AccountId> {
		#[allow(dead_code)]
		#[doc(hidden)]
		pub fn metadata() -> &'static [::frame_support::event::EventMetadata] {
			&[
				::frame_support::event::EventMetadata {
					name: ::frame_support::event::DecodeDifferent::Encode("SolutionStored"),
					arguments: ::frame_support::event::DecodeDifferent::Encode(&[
						"ElectionCompute",
					]),
					documentation: ::frame_support::event::DecodeDifferent::Encode(&[
						r" A solution was stored with the given compute.",
						r"",
						r" If the solution is signed, this means that it hasn't yet been processed. If the solution",
						r" is unsigned, this means that it has also been processed.",
					]),
				},
				::frame_support::event::EventMetadata {
					name: ::frame_support::event::DecodeDifferent::Encode("ElectionFinalized"),
					arguments: ::frame_support::event::DecodeDifferent::Encode(&[
						"Option<ElectionCompute>",
					]),
					documentation: ::frame_support::event::DecodeDifferent::Encode(&[
						r" The election has been finalized, with `Some` of the given computation, or else if the",
						r" election failed, `None`.",
					]),
				},
				::frame_support::event::EventMetadata {
					name: ::frame_support::event::DecodeDifferent::Encode("Rewarded"),
					arguments: ::frame_support::event::DecodeDifferent::Encode(&["AccountId"]),
					documentation: ::frame_support::event::DecodeDifferent::Encode(&[
						r" An account has been rewarded for their signed submission being finalized.",
					]),
				},
				::frame_support::event::EventMetadata {
					name: ::frame_support::event::DecodeDifferent::Encode("Slashed"),
					arguments: ::frame_support::event::DecodeDifferent::Encode(&["AccountId"]),
					documentation: ::frame_support::event::DecodeDifferent::Encode(&[
						r" An account has been slashed for submitting an invalid signed submission.",
					]),
				},
			]
		}
	}
	pub enum PalletError<T: Trait>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[doc(hidden)]
		__Ignore(
			::frame_support::sp_std::marker::PhantomData<(T,)>,
			::frame_support::Never,
		),
		/// Submission was too early.
		EarlySubmission,
		/// Submission was too weak, score-wise.
		WeakSubmission,
		/// The queue was full, and the solution was not better than any of the existing ones.
		QueueFull,
		/// The origin failed to pay the deposit.
		CannotPayDeposit,
	}
	impl<T: Trait> ::frame_support::sp_std::fmt::Debug for PalletError<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn fmt(
			&self,
			f: &mut ::frame_support::sp_std::fmt::Formatter<'_>,
		) -> ::frame_support::sp_std::fmt::Result {
			f.write_str(self.as_str())
		}
	}
	impl<T: Trait> PalletError<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn as_u8(&self) -> u8 {
			match self {
				PalletError::__Ignore(_, _) => {
					::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
						&["internal error: entered unreachable code: "],
						&match (&"`__Ignore` can never be constructed",) {
							(arg0,) => [::core::fmt::ArgumentV1::new(
								arg0,
								::core::fmt::Display::fmt,
							)],
						},
					))
				}
				PalletError::EarlySubmission => 0,
				PalletError::WeakSubmission => 0 + 1,
				PalletError::QueueFull => 0 + 1 + 1,
				PalletError::CannotPayDeposit => 0 + 1 + 1 + 1,
			}
		}
		fn as_str(&self) -> &'static str {
			match self {
				Self::__Ignore(_, _) => {
					::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
						&["internal error: entered unreachable code: "],
						&match (&"`__Ignore` can never be constructed",) {
							(arg0,) => [::core::fmt::ArgumentV1::new(
								arg0,
								::core::fmt::Display::fmt,
							)],
						},
					))
				}
				PalletError::EarlySubmission => "EarlySubmission",
				PalletError::WeakSubmission => "WeakSubmission",
				PalletError::QueueFull => "QueueFull",
				PalletError::CannotPayDeposit => "CannotPayDeposit",
			}
		}
	}
	impl<T: Trait> From<PalletError<T>> for &'static str
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn from(err: PalletError<T>) -> &'static str {
			err.as_str()
		}
	}
	impl<T: Trait> From<PalletError<T>> for ::frame_support::sp_runtime::DispatchError
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn from(err: PalletError<T>) -> Self {
			let index = <T::PalletInfo as ::frame_support::traits::PalletInfo>::index::<Module<T>>()
				.expect("Every active module has an index in the runtime; qed") as u8;
			::frame_support::sp_runtime::DispatchError::Module {
				index,
				error: err.as_u8(),
				message: Some(err.as_str()),
			}
		}
	}
	impl<T: Trait> ::frame_support::error::ModuleErrorMetadata for PalletError<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn metadata() -> &'static [::frame_support::error::ErrorMetadata] {
			&[
				::frame_support::error::ErrorMetadata {
					name: ::frame_support::error::DecodeDifferent::Encode("EarlySubmission"),
					documentation: ::frame_support::error::DecodeDifferent::Encode(&[
						r" Submission was too early.",
					]),
				},
				::frame_support::error::ErrorMetadata {
					name: ::frame_support::error::DecodeDifferent::Encode("WeakSubmission"),
					documentation: ::frame_support::error::DecodeDifferent::Encode(&[
						r" Submission was too weak, score-wise.",
					]),
				},
				::frame_support::error::ErrorMetadata {
					name: ::frame_support::error::DecodeDifferent::Encode("QueueFull"),
					documentation: ::frame_support::error::DecodeDifferent::Encode(&[
						r" The queue was full, and the solution was not better than any of the existing ones.",
					]),
				},
				::frame_support::error::ErrorMetadata {
					name: ::frame_support::error::DecodeDifferent::Encode("CannotPayDeposit"),
					documentation: ::frame_support::error::DecodeDifferent::Encode(&[
						r" The origin failed to pay the deposit.",
					]),
				},
			]
		}
	}
	pub struct Module<T: Trait>(::frame_support::sp_std::marker::PhantomData<(T,)>)
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>;
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<T: ::core::clone::Clone + Trait> ::core::clone::Clone for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[inline]
		fn clone(&self) -> Module<T> {
			match *self {
				Module(ref __self_0_0) => Module(::core::clone::Clone::clone(&(*__self_0_0))),
			}
		}
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<T: ::core::marker::Copy + Trait> ::core::marker::Copy for Module<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	impl<T: Trait> ::core::marker::StructuralPartialEq for Module<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<T: ::core::cmp::PartialEq + Trait> ::core::cmp::PartialEq for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[inline]
		fn eq(&self, other: &Module<T>) -> bool {
			match *other {
				Module(ref __self_1_0) => match *self {
					Module(ref __self_0_0) => (*__self_0_0) == (*__self_1_0),
				},
			}
		}
		#[inline]
		fn ne(&self, other: &Module<T>) -> bool {
			match *other {
				Module(ref __self_1_0) => match *self {
					Module(ref __self_0_0) => (*__self_0_0) != (*__self_1_0),
				},
			}
		}
	}
	impl<T: Trait> ::core::marker::StructuralEq for Module<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	#[automatically_derived]
	#[allow(unused_qualifications)]
	impl<T: ::core::cmp::Eq + Trait> ::core::cmp::Eq for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[inline]
		#[doc(hidden)]
		fn assert_receiver_is_total_eq(&self) -> () {
			{
				let _: ::core::cmp::AssertParamIsEq<
					::frame_support::sp_std::marker::PhantomData<(T,)>,
				>;
			}
		}
	}
	impl<T: Trait> core::fmt::Debug for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
		T: core::fmt::Debug,
	{
		fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
			fmt.debug_tuple("Module").field(&self.0).finish()
		}
	}
	impl<T: frame_system::Trait + Trait>
		::frame_support::traits::OnInitialize<<T as frame_system::Trait>::BlockNumber> for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn on_initialize(now: T::BlockNumber) -> Weight {
			let __within_span__ = {
				use ::tracing::__macro_support::Callsite as _;
				static CALLSITE: ::tracing::__macro_support::MacroCallsite = {
					use ::tracing::__macro_support::MacroCallsite;
					static META: ::tracing::Metadata<'static> = {
						::tracing_core::metadata::Metadata::new(
							"on_initialize",
							"frame_election_providers::two_phase",
							::tracing::Level::TRACE,
							Some("frame/election-providers/src/two_phase/mod.rs"),
							Some(464u32),
							Some("frame_election_providers::two_phase"),
							::tracing_core::field::FieldSet::new(
								&[],
								::tracing_core::callsite::Identifier(&CALLSITE),
							),
							::tracing::metadata::Kind::SPAN,
						)
					};
					MacroCallsite::new(&META)
				};
				let mut interest = ::tracing::subscriber::Interest::never();
				if ::tracing::Level::TRACE <= ::tracing::level_filters::STATIC_MAX_LEVEL
					&& ::tracing::Level::TRACE <= ::tracing::level_filters::LevelFilter::current()
					&& {
						interest = CALLSITE.interest();
						!interest.is_never()
					} && CALLSITE.is_enabled(interest)
				{
					let meta = CALLSITE.metadata();
					::tracing::Span::new(meta, &{ meta.fields().value_set(&[]) })
				} else {
					let span = CALLSITE.disabled_span();
					{};
					span
				}
			};
			let __tracing_guard__ = __within_span__.enter();
			{
				let next_election = T::ElectionDataProvider::next_election_prediction(now);
				let next_election = next_election.max(now);
				let signed_deadline = T::SignedPhase::get() + T::UnsignedPhase::get();
				let unsigned_deadline = T::UnsignedPhase::get();
				let remaining = next_election - now;
				match Self::current_phase() {
					Phase::Off if remaining <= signed_deadline && remaining > unsigned_deadline => {
						<CurrentPhase<T>>::put(Phase::Signed);
						Round::mutate(|r| *r += 1);
						Self::start_signed_phase();
						{
							let lvl = ::log::Level::Info;
							if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
								::log::__private_api_log(
									::core::fmt::Arguments::new_v1(
										&["\u{1f3e6} Starting signed phase at #", " , round "],
										&match (&now, &Self::round()) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Display::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Display::fmt,
												),
											],
										},
									),
									lvl,
									&(
										crate::LOG_TARGET,
										"frame_election_providers::two_phase",
										"frame/election-providers/src/two_phase/mod.rs",
										488u32,
									),
								);
							}
						};
					}
					Phase::Signed if remaining <= unsigned_deadline && remaining > 0.into() => {
						let found_solution = Self::finalize_signed_phase();
						<CurrentPhase<T>>::put(Phase::Unsigned((!found_solution, now)));
						{
							let lvl = ::log::Level::Info;
							if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
								::log::__private_api_log(
									::core::fmt::Arguments::new_v1(
										&["\u{1f3e6} Starting unsigned phase at #"],
										&match (&now,) {
											(arg0,) => [::core::fmt::ArgumentV1::new(
												arg0,
												::core::fmt::Display::fmt,
											)],
										},
									),
									lvl,
									&(
										crate::LOG_TARGET,
										"frame_election_providers::two_phase",
										"frame/election-providers/src/two_phase/mod.rs",
										497u32,
									),
								);
							}
						};
					}
					_ => {}
				}
				Default::default()
			}
		}
	}
	impl<T: Trait> ::frame_support::traits::OnRuntimeUpgrade for Module<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	impl<T: frame_system::Trait + Trait>
		::frame_support::traits::OnFinalize<<T as frame_system::Trait>::BlockNumber> for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
	}
	impl<T: frame_system::Trait + Trait>
		::frame_support::traits::OffchainWorker<<T as frame_system::Trait>::BlockNumber> for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn offchain_worker(n: T::BlockNumber) {
			if Self::set_check_offchain_execution_status(n).is_ok()
				&& Self::current_phase().is_unsigned_open_at(n)
			{
				let _ = Self::mine_and_submit().map_err(|e| {
					let lvl = ::log::Level::Error;
					if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
						::log::__private_api_log(
							::core::fmt::Arguments::new_v1(
								&["\u{1f3e6} error while submitting transaction in OCW: "],
								&match (&e,) {
									(arg0,) => [::core::fmt::ArgumentV1::new(
										arg0,
										::core::fmt::Debug::fmt,
									)],
								},
							),
							lvl,
							&(
								crate::LOG_TARGET,
								"frame_election_providers::two_phase",
								"frame/election-providers/src/two_phase/mod.rs",
								514u32,
							),
						);
					}
				});
			}
		}
	}
	impl<T: Trait> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		/// Deposits an event using `frame_system::Module::deposit_event`.
		fn deposit_event(event: impl Into<<T as Trait>::Event>) {
			<frame_system::Module<T>>::deposit_event(event.into())
		}
	}
	#[cfg(feature = "std")]
	impl<T: Trait> ::frame_support::traits::IntegrityTest for Module<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	/// Can also be called using [`Call`].
	///
	/// [`Call`]: enum.Call.html
	impl<T: Trait> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		/// Submit a solution for the signed phase.
		///
		/// The dispatch origin fo this call must be __signed__.
		///
		/// The solution potentially queued, based on the claimed score and processed at the end of
		/// the signed phase.
		///
		/// A deposit is reserved and recorded for the solution. Based on the outcome, the solution
		/// might be rewarded, slashed, or get all or a part of the deposit back.
		///
		/// NOTE: Calling this function will bypass origin filters.
		fn submit(
			origin: T::Origin,
			solution: RawSolution<CompactOf<T>>,
		) -> DispatchResultWithPostInfo {
			let __within_span__ = {
				use ::tracing::__macro_support::Callsite as _;
				static CALLSITE: ::tracing::__macro_support::MacroCallsite = {
					use ::tracing::__macro_support::MacroCallsite;
					static META: ::tracing::Metadata<'static> = {
						::tracing_core::metadata::Metadata::new(
							"submit",
							"frame_election_providers::two_phase",
							::tracing::Level::TRACE,
							Some("frame/election-providers/src/two_phase/mod.rs"),
							Some(464u32),
							Some("frame_election_providers::two_phase"),
							::tracing_core::field::FieldSet::new(
								&[],
								::tracing_core::callsite::Identifier(&CALLSITE),
							),
							::tracing::metadata::Kind::SPAN,
						)
					};
					MacroCallsite::new(&META)
				};
				let mut interest = ::tracing::subscriber::Interest::never();
				if ::tracing::Level::TRACE <= ::tracing::level_filters::STATIC_MAX_LEVEL
					&& ::tracing::Level::TRACE <= ::tracing::level_filters::LevelFilter::current()
					&& {
						interest = CALLSITE.interest();
						!interest.is_never()
					} && CALLSITE.is_enabled(interest)
				{
					let meta = CALLSITE.metadata();
					::tracing::Span::new(meta, &{ meta.fields().value_set(&[]) })
				} else {
					let span = CALLSITE.disabled_span();
					{};
					span
				}
			};
			let __tracing_guard__ = __within_span__.enter();
			let who = ensure_signed(origin)?;
			{
				if !Self::current_phase().is_signed() {
					{
						return Err(PalletError::<T>::EarlySubmission.into());
					};
				}
			};
			let mut signed_submissions = Self::signed_submissions();
			let maybe_index = Self::insert_submission(&who, &mut signed_submissions, solution);
			{
				if !maybe_index.is_some() {
					{
						return Err("QueueFull".into());
					};
				}
			};
			let index = maybe_index.expect("Option checked to be `Some`; qed.");
			let deposit = signed_submissions[index].deposit;
			T::Currency::reserve(&who, deposit).map_err(|_| PalletError::<T>::CannotPayDeposit)?;
			if true {
				if !(signed_submissions.len() as u32 <= T::MaxSignedSubmissions::get()) {
					{
						:: std :: rt :: begin_panic ( "assertion failed: signed_submissions.len() as u32 <= T::MaxSignedSubmissions::get()" )
					}
				};
			};
			<SignedSubmissions<T>>::put(signed_submissions);
			Self::deposit_event(RawEvent::SolutionStored(ElectionCompute::Signed));
			Ok(None.into())
		}
		#[allow(unreachable_code)]
		/// Submit a solution for the unsigned phase.
		///
		/// The dispatch origin fo this call must be __signed__.
		///
		/// This submission is checked on the fly, thus it is likely yo be more limited and smaller.
		/// Moreover, this unsigned solution is only validated when submitted to the pool from the
		/// local process. Effectively, this means that only active validators can submit this
		/// transaction when authoring a block.
		///
		/// To prevent any incorrect solution (and thus wasted time/weight), this transaction will
		/// panic if the solution submitted by the validator is invalid, effectively putting their
		/// authoring reward at risk.
		///
		/// No deposit or reward is associated with this.
		///
		/// NOTE: Calling this function will bypass origin filters.
		fn submit_unsigned(
			origin: T::Origin,
			solution: RawSolution<CompactOf<T>>,
		) -> ::frame_support::dispatch::DispatchResult {
			let __within_span__ = {
				use ::tracing::__macro_support::Callsite as _;
				static CALLSITE: ::tracing::__macro_support::MacroCallsite = {
					use ::tracing::__macro_support::MacroCallsite;
					static META: ::tracing::Metadata<'static> = {
						::tracing_core::metadata::Metadata::new(
							"submit_unsigned",
							"frame_election_providers::two_phase",
							::tracing::Level::TRACE,
							Some("frame/election-providers/src/two_phase/mod.rs"),
							Some(464u32),
							Some("frame_election_providers::two_phase"),
							::tracing_core::field::FieldSet::new(
								&[],
								::tracing_core::callsite::Identifier(&CALLSITE),
							),
							::tracing::metadata::Kind::SPAN,
						)
					};
					MacroCallsite::new(&META)
				};
				let mut interest = ::tracing::subscriber::Interest::never();
				if ::tracing::Level::TRACE <= ::tracing::level_filters::STATIC_MAX_LEVEL
					&& ::tracing::Level::TRACE <= ::tracing::level_filters::LevelFilter::current()
					&& {
						interest = CALLSITE.interest();
						!interest.is_never()
					} && CALLSITE.is_enabled(interest)
				{
					let meta = CALLSITE.metadata();
					::tracing::Span::new(meta, &{ meta.fields().value_set(&[]) })
				} else {
					let span = CALLSITE.disabled_span();
					{};
					span
				}
			};
			let __tracing_guard__ = __within_span__.enter();
			{
				ensure_none(origin)?;
				let _ = Self::pre_dispatch_checks(&solution)?;
				let ready = Self::feasibility_check(solution, ElectionCompute::Unsigned).expect(
					"Invalid unsigned submission must produce invalid block and deprive \
						validator from their authoring reward.",
				);
				<QueuedSolution<T>>::put(ready);
				Self::deposit_event(RawEvent::SolutionStored(ElectionCompute::Unsigned));
			}
			Ok(())
		}
	}
	/// Dispatchable calls.
	///
	/// Each variant of this enum maps to a dispatchable function from the associated module.
	pub enum Call<T: Trait>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[doc(hidden)]
		#[codec(skip)]
		__PhantomItem(
			::frame_support::sp_std::marker::PhantomData<(T,)>,
			::frame_support::Never,
		),
		#[allow(non_camel_case_types)]
		/// Submit a solution for the signed phase.
		///
		/// The dispatch origin fo this call must be __signed__.
		///
		/// The solution potentially queued, based on the claimed score and processed at the end of
		/// the signed phase.
		///
		/// A deposit is reserved and recorded for the solution. Based on the outcome, the solution
		/// might be rewarded, slashed, or get all or a part of the deposit back.
		submit(RawSolution<CompactOf<T>>),
		#[allow(non_camel_case_types)]
		/// Submit a solution for the unsigned phase.
		///
		/// The dispatch origin fo this call must be __signed__.
		///
		/// This submission is checked on the fly, thus it is likely yo be more limited and smaller.
		/// Moreover, this unsigned solution is only validated when submitted to the pool from the
		/// local process. Effectively, this means that only active validators can submit this
		/// transaction when authoring a block.
		///
		/// To prevent any incorrect solution (and thus wasted time/weight), this transaction will
		/// panic if the solution submitted by the validator is invalid, effectively putting their
		/// authoring reward at risk.
		///
		/// No deposit or reward is associated with this.
		submit_unsigned(RawSolution<CompactOf<T>>),
	}
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<T: Trait> _parity_scale_codec::Encode for Call<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
		{
			fn encode_to<EncOut: _parity_scale_codec::Output>(&self, dest: &mut EncOut) {
				match *self {
					Call::submit(ref aa) => {
						dest.push_byte(0usize as u8);
						dest.push(aa);
					}
					Call::submit_unsigned(ref aa) => {
						dest.push_byte(1usize as u8);
						dest.push(aa);
					}
					_ => (),
				}
			}
		}
		impl<T: Trait> _parity_scale_codec::EncodeLike for Call<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Encode,
		{
		}
	};
	const _: () = {
		#[allow(unknown_lints)]
		#[allow(rust_2018_idioms)]
		extern crate codec as _parity_scale_codec;
		impl<T: Trait> _parity_scale_codec::Decode for Call<T>
		where
			ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Decode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Decode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Decode,
			RawSolution<CompactOf<T>>: _parity_scale_codec::Decode,
		{
			fn decode<DecIn: _parity_scale_codec::Input>(
				input: &mut DecIn,
			) -> core::result::Result<Self, _parity_scale_codec::Error> {
				match input.read_byte()? {
					x if x == 0usize as u8 => Ok(Call::submit({
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => return Err("Error decoding field Call :: submit.0".into()),
							Ok(a) => a,
						}
					})),
					x if x == 1usize as u8 => Ok(Call::submit_unsigned({
						let res = _parity_scale_codec::Decode::decode(input);
						match res {
							Err(_) => {
								return Err("Error decoding field Call :: submit_unsigned.0".into())
							}
							Ok(a) => a,
						}
					})),
					x => Err("No such variant in enum Call".into()),
				}
			}
		}
	};
	impl<T: Trait> ::frame_support::dispatch::GetDispatchInfo for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn get_dispatch_info(&self) -> ::frame_support::dispatch::DispatchInfo {
			match *self {
				Call::submit(ref solution) => {
					let base_weight = T::WeightInfo::submit();
					let weight = <dyn ::frame_support::dispatch::WeighData<(
						&RawSolution<CompactOf<T>>,
					)>>::weigh_data(&base_weight, (solution,));
					let class = <dyn ::frame_support::dispatch::ClassifyDispatch<(
						&RawSolution<CompactOf<T>>,
					)>>::classify_dispatch(&base_weight, (solution,));
					let pays_fee = <dyn ::frame_support::dispatch::PaysFee<(
						&RawSolution<CompactOf<T>>,
					)>>::pays_fee(&base_weight, (solution,));
					::frame_support::dispatch::DispatchInfo {
						weight,
						class,
						pays_fee,
					}
				}
				Call::submit_unsigned(ref solution) => {
					let base_weight = T::WeightInfo::submit_unsigned();
					let weight = <dyn ::frame_support::dispatch::WeighData<(
						&RawSolution<CompactOf<T>>,
					)>>::weigh_data(&base_weight, (solution,));
					let class = <dyn ::frame_support::dispatch::ClassifyDispatch<(
						&RawSolution<CompactOf<T>>,
					)>>::classify_dispatch(&base_weight, (solution,));
					let pays_fee = <dyn ::frame_support::dispatch::PaysFee<(
						&RawSolution<CompactOf<T>>,
					)>>::pays_fee(&base_weight, (solution,));
					::frame_support::dispatch::DispatchInfo {
						weight,
						class,
						pays_fee,
					}
				}
				Call::__PhantomItem(_, _) => {
					::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
						&["internal error: entered unreachable code: "],
						&match (&"__PhantomItem should never be used.",) {
							(arg0,) => [::core::fmt::ArgumentV1::new(
								arg0,
								::core::fmt::Display::fmt,
							)],
						},
					))
				}
			}
		}
	}
	impl<T: Trait> ::frame_support::dispatch::GetCallName for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn get_call_name(&self) -> &'static str {
			match *self {
				Call::submit(ref solution) => {
					let _ = (solution);
					"submit"
				}
				Call::submit_unsigned(ref solution) => {
					let _ = (solution);
					"submit_unsigned"
				}
				Call::__PhantomItem(_, _) => {
					::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
						&["internal error: entered unreachable code: "],
						&match (&"__PhantomItem should never be used.",) {
							(arg0,) => [::core::fmt::ArgumentV1::new(
								arg0,
								::core::fmt::Display::fmt,
							)],
						},
					))
				}
			}
		}
		fn get_call_names() -> &'static [&'static str] {
			&["submit", "submit_unsigned"]
		}
	}
	impl<T: Trait> ::frame_support::dispatch::Clone for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn clone(&self) -> Self {
			match *self {
				Call::submit(ref solution) => Call::submit((*solution).clone()),
				Call::submit_unsigned(ref solution) => Call::submit_unsigned((*solution).clone()),
				_ => ::std::rt::begin_panic("internal error: entered unreachable code"),
			}
		}
	}
	impl<T: Trait> ::frame_support::dispatch::PartialEq for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn eq(&self, _other: &Self) -> bool {
			match *self {
				Call::submit(ref solution) => {
					let self_params = (solution,);
					if let Call::submit(ref solution) = *_other {
						self_params == (solution,)
					} else {
						match *_other {
							Call::__PhantomItem(_, _) => {
								::std::rt::begin_panic("internal error: entered unreachable code")
							}
							_ => false,
						}
					}
				}
				Call::submit_unsigned(ref solution) => {
					let self_params = (solution,);
					if let Call::submit_unsigned(ref solution) = *_other {
						self_params == (solution,)
					} else {
						match *_other {
							Call::__PhantomItem(_, _) => {
								::std::rt::begin_panic("internal error: entered unreachable code")
							}
							_ => false,
						}
					}
				}
				_ => ::std::rt::begin_panic("internal error: entered unreachable code"),
			}
		}
	}
	impl<T: Trait> ::frame_support::dispatch::Eq for Call<T> where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>
	{
	}
	impl<T: Trait> ::frame_support::dispatch::fmt::Debug for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn fmt(
			&self,
			_f: &mut ::frame_support::dispatch::fmt::Formatter,
		) -> ::frame_support::dispatch::result::Result<(), ::frame_support::dispatch::fmt::Error> {
			match *self {
				Call::submit(ref solution) => _f.write_fmt(::core::fmt::Arguments::new_v1(
					&["", ""],
					&match (&"submit", &(solution.clone(),)) {
						(arg0, arg1) => [
							::core::fmt::ArgumentV1::new(arg0, ::core::fmt::Display::fmt),
							::core::fmt::ArgumentV1::new(arg1, ::core::fmt::Debug::fmt),
						],
					},
				)),
				Call::submit_unsigned(ref solution) => {
					_f.write_fmt(::core::fmt::Arguments::new_v1(
						&["", ""],
						&match (&"submit_unsigned", &(solution.clone(),)) {
							(arg0, arg1) => [
								::core::fmt::ArgumentV1::new(arg0, ::core::fmt::Display::fmt),
								::core::fmt::ArgumentV1::new(arg1, ::core::fmt::Debug::fmt),
							],
						},
					))
				}
				_ => ::std::rt::begin_panic("internal error: entered unreachable code"),
			}
		}
	}
	impl<T: Trait> ::frame_support::traits::UnfilteredDispatchable for Call<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Origin = T::Origin;
		fn dispatch_bypass_filter(
			self,
			_origin: Self::Origin,
		) -> ::frame_support::dispatch::DispatchResultWithPostInfo {
			match self {
				Call::submit(solution) => <Module<T>>::submit(_origin, solution)
					.map(Into::into)
					.map_err(Into::into),
				Call::submit_unsigned(solution) => <Module<T>>::submit_unsigned(_origin, solution)
					.map(Into::into)
					.map_err(Into::into),
				Call::__PhantomItem(_, _) => {
					::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
						&["internal error: entered unreachable code: "],
						&match (&"__PhantomItem should never be used.",) {
							(arg0,) => [::core::fmt::ArgumentV1::new(
								arg0,
								::core::fmt::Display::fmt,
							)],
						},
					))
				}
			}
		}
	}
	impl<T: Trait> ::frame_support::dispatch::Callable<T> for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		type Call = Call<T>;
	}
	impl<T: Trait> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[doc(hidden)]
		#[allow(dead_code)]
		pub fn call_functions() -> &'static [::frame_support::dispatch::FunctionMetadata] {
			&[
				::frame_support::dispatch::FunctionMetadata {
					name: ::frame_support::dispatch::DecodeDifferent::Encode("submit"),
					arguments: ::frame_support::dispatch::DecodeDifferent::Encode(&[
						::frame_support::dispatch::FunctionArgumentMetadata {
							name: ::frame_support::dispatch::DecodeDifferent::Encode("solution"),
							ty: ::frame_support::dispatch::DecodeDifferent::Encode(
								"RawSolution<CompactOf<T>>",
							),
						},
					]),
					documentation: ::frame_support::dispatch::DecodeDifferent::Encode(&[
						r" Submit a solution for the signed phase.",
						r"",
						r" The dispatch origin fo this call must be __signed__.",
						r"",
						r" The solution potentially queued, based on the claimed score and processed at the end of",
						r" the signed phase.",
						r"",
						r" A deposit is reserved and recorded for the solution. Based on the outcome, the solution",
						r" might be rewarded, slashed, or get all or a part of the deposit back.",
					]),
				},
				::frame_support::dispatch::FunctionMetadata {
					name: ::frame_support::dispatch::DecodeDifferent::Encode("submit_unsigned"),
					arguments: ::frame_support::dispatch::DecodeDifferent::Encode(&[
						::frame_support::dispatch::FunctionArgumentMetadata {
							name: ::frame_support::dispatch::DecodeDifferent::Encode("solution"),
							ty: ::frame_support::dispatch::DecodeDifferent::Encode(
								"RawSolution<CompactOf<T>>",
							),
						},
					]),
					documentation: ::frame_support::dispatch::DecodeDifferent::Encode(&[
						r" Submit a solution for the unsigned phase.",
						r"",
						r" The dispatch origin fo this call must be __signed__.",
						r"",
						r" This submission is checked on the fly, thus it is likely yo be more limited and smaller.",
						r" Moreover, this unsigned solution is only validated when submitted to the pool from the",
						r" local process. Effectively, this means that only active validators can submit this",
						r" transaction when authoring a block.",
						r"",
						r" To prevent any incorrect solution (and thus wasted time/weight), this transaction will",
						r" panic if the solution submitted by the validator is invalid, effectively putting their",
						r" authoring reward at risk.",
						r"",
						r" No deposit or reward is associated with this.",
					]),
				},
			]
		}
	}
	impl<T: 'static + Trait> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		#[doc(hidden)]
		#[allow(dead_code)]
		pub fn module_constants_metadata(
		) -> &'static [::frame_support::dispatch::ModuleConstantMetadata] {
			&[]
		}
	}
	impl<T: Trait> ::frame_support::dispatch::ModuleErrorMetadata for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		fn metadata() -> &'static [::frame_support::dispatch::ErrorMetadata] {
			<PalletError<T> as ::frame_support::dispatch::ModuleErrorMetadata>::metadata()
		}
	}
	impl<T: Trait> Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		/// Checks the feasibility of a solution.
		///
		/// This checks the solution for the following:
		///
		/// 0. **all** of the used indices must be correct.
		/// 1. present correct number of winners.
		/// 2. any assignment is checked to match with `SnapshotVoters`.
		/// 3. for each assignment, the check of `ElectionDataProvider` is also examined.
		/// 4. the claimed score is valid.
		fn feasibility_check(
			solution: RawSolution<CompactOf<T>>,
			compute: ElectionCompute,
		) -> Result<ReadySolution<T::AccountId>, FeasibilityError> {
			let RawSolution { compact, score } = solution;
			let winners = compact.unique_targets();
			{
				if !(winners.len() as u32 == Self::desired_targets()) {
					{
						return Err(FeasibilityError::WrongWinnerCount.into());
					};
				}
			};
			let snapshot_voters =
				Self::snapshot_voters().ok_or(FeasibilityError::SnapshotUnavailable)?;
			let snapshot_targets =
				Self::snapshot_targets().ok_or(FeasibilityError::SnapshotUnavailable)?;
			let voter_at = |i: crate::two_phase::CompactVoterIndexOf<T>| -> Option<T::AccountId> {
				<crate::two_phase::CompactVoterIndexOf<T> as crate::TryInto<usize>>::try_into(i)
					.ok()
					.and_then(|i| snapshot_voters.get(i).map(|(x, _, _)| x).cloned())
			};
			let target_at = |i: crate::two_phase::CompactTargetIndexOf<T>| -> Option<T::AccountId> {
				<crate::two_phase::CompactTargetIndexOf<T> as crate::TryInto<usize>>::try_into(i)
					.ok()
					.and_then(|i| snapshot_targets.get(i).cloned())
			};
			let winners = winners
				.into_iter()
				.map(|i| target_at(i).ok_or(FeasibilityError::InvalidWinner))
				.collect::<Result<Vec<T::AccountId>, FeasibilityError>>()?;
			let assignments = compact
				.into_assignment(voter_at, target_at)
				.map_err::<FeasibilityError, _>(Into::into)?;
			let _ = assignments
				.iter()
				.map(|Assignment { who, distribution }| {
					snapshot_voters.iter().find(|(v, _, _)| v == who).map_or(
						Err(FeasibilityError::InvalidVoter),
						|(_, _, t)| {
							if distribution.iter().map(|(x, _)| x).all(|x| t.contains(x))
								&& T::ElectionDataProvider::feasibility_check_assignment::<
									CompactAccuracyOf<T>,
								>(who, distribution)
							{
								Ok(())
							} else {
								Err(FeasibilityError::InvalidVote)
							}
						},
					)
				})
				.collect::<Result<(), FeasibilityError>>()?;
			let stake_of = |who: &T::AccountId| -> crate::VoteWeight {
				snapshot_voters
					.iter()
					.find(|(x, _, _)| x == who)
					.map(|(_, x, _)| *x)
					.unwrap_or_default()
			};
			let staked_assignments = assignment_ratio_to_staked_normalized(assignments, stake_of)
				.map_err::<FeasibilityError, _>(Into::into)?;
			let supports = sp_npos_elections::to_supports(&winners, &staked_assignments)
				.map_err::<FeasibilityError, _>(Into::into)?;
			let known_score = supports.evaluate();
			(
				match known_score {
					tmp => {
						{
							::std::io::_eprint(::core::fmt::Arguments::new_v1_formatted(
								&["[", ":", "] ", " = ", "\n"],
								&match (
									&"frame/election-providers/src/two_phase/mod.rs",
									&674u32,
									&"known_score",
									&&tmp,
								) {
									(arg0, arg1, arg2, arg3) => [
										::core::fmt::ArgumentV1::new(
											arg0,
											::core::fmt::Display::fmt,
										),
										::core::fmt::ArgumentV1::new(
											arg1,
											::core::fmt::Display::fmt,
										),
										::core::fmt::ArgumentV1::new(
											arg2,
											::core::fmt::Display::fmt,
										),
										::core::fmt::ArgumentV1::new(arg3, ::core::fmt::Debug::fmt),
									],
								},
								&[
									::core::fmt::rt::v1::Argument {
										position: 0usize,
										format: ::core::fmt::rt::v1::FormatSpec {
											fill: ' ',
											align: ::core::fmt::rt::v1::Alignment::Unknown,
											flags: 0u32,
											precision: ::core::fmt::rt::v1::Count::Implied,
											width: ::core::fmt::rt::v1::Count::Implied,
										},
									},
									::core::fmt::rt::v1::Argument {
										position: 1usize,
										format: ::core::fmt::rt::v1::FormatSpec {
											fill: ' ',
											align: ::core::fmt::rt::v1::Alignment::Unknown,
											flags: 0u32,
											precision: ::core::fmt::rt::v1::Count::Implied,
											width: ::core::fmt::rt::v1::Count::Implied,
										},
									},
									::core::fmt::rt::v1::Argument {
										position: 2usize,
										format: ::core::fmt::rt::v1::FormatSpec {
											fill: ' ',
											align: ::core::fmt::rt::v1::Alignment::Unknown,
											flags: 0u32,
											precision: ::core::fmt::rt::v1::Count::Implied,
											width: ::core::fmt::rt::v1::Count::Implied,
										},
									},
									::core::fmt::rt::v1::Argument {
										position: 3usize,
										format: ::core::fmt::rt::v1::FormatSpec {
											fill: ' ',
											align: ::core::fmt::rt::v1::Alignment::Unknown,
											flags: 4u32,
											precision: ::core::fmt::rt::v1::Count::Implied,
											width: ::core::fmt::rt::v1::Count::Implied,
										},
									},
								],
							));
						};
						tmp
					}
				},
				match score {
					tmp => {
						{
							::std::io::_eprint(::core::fmt::Arguments::new_v1_formatted(
								&["[", ":", "] ", " = ", "\n"],
								&match (
									&"frame/election-providers/src/two_phase/mod.rs",
									&674u32,
									&"score",
									&&tmp,
								) {
									(arg0, arg1, arg2, arg3) => [
										::core::fmt::ArgumentV1::new(
											arg0,
											::core::fmt::Display::fmt,
										),
										::core::fmt::ArgumentV1::new(
											arg1,
											::core::fmt::Display::fmt,
										),
										::core::fmt::ArgumentV1::new(
											arg2,
											::core::fmt::Display::fmt,
										),
										::core::fmt::ArgumentV1::new(arg3, ::core::fmt::Debug::fmt),
									],
								},
								&[
									::core::fmt::rt::v1::Argument {
										position: 0usize,
										format: ::core::fmt::rt::v1::FormatSpec {
											fill: ' ',
											align: ::core::fmt::rt::v1::Alignment::Unknown,
											flags: 0u32,
											precision: ::core::fmt::rt::v1::Count::Implied,
											width: ::core::fmt::rt::v1::Count::Implied,
										},
									},
									::core::fmt::rt::v1::Argument {
										position: 1usize,
										format: ::core::fmt::rt::v1::FormatSpec {
											fill: ' ',
											align: ::core::fmt::rt::v1::Alignment::Unknown,
											flags: 0u32,
											precision: ::core::fmt::rt::v1::Count::Implied,
											width: ::core::fmt::rt::v1::Count::Implied,
										},
									},
									::core::fmt::rt::v1::Argument {
										position: 2usize,
										format: ::core::fmt::rt::v1::FormatSpec {
											fill: ' ',
											align: ::core::fmt::rt::v1::Alignment::Unknown,
											flags: 0u32,
											precision: ::core::fmt::rt::v1::Count::Implied,
											width: ::core::fmt::rt::v1::Count::Implied,
										},
									},
									::core::fmt::rt::v1::Argument {
										position: 3usize,
										format: ::core::fmt::rt::v1::FormatSpec {
											fill: ' ',
											align: ::core::fmt::rt::v1::Alignment::Unknown,
											flags: 4u32,
											precision: ::core::fmt::rt::v1::Count::Implied,
											width: ::core::fmt::rt::v1::Count::Implied,
										},
									},
								],
							));
						};
						tmp
					}
				},
			);
			{
				if !(known_score == score) {
					{
						return Err(FeasibilityError::InvalidScore.into());
					};
				}
			};
			Ok(ReadySolution {
				supports,
				compute,
				score,
			})
		}
		/// On-chain fallback of election.
		fn onchain_fallback() -> Result<Supports<T::AccountId>, Error> {
			let desired_targets = Self::desired_targets() as usize;
			let voters = Self::snapshot_voters().ok_or(Error::SnapshotUnAvailable)?;
			let targets = Self::snapshot_targets().ok_or(Error::SnapshotUnAvailable)?;
			<OnChainSequentialPhragmen as ElectionProvider<T::AccountId>>::elect::<Perbill>(
				desired_targets,
				targets,
				voters,
			)
			.map_err(Into::into)
		}
	}
	impl<T: Trait> ElectionProvider<T::AccountId> for Module<T>
	where
		ExtendedBalance: From<InnerOf<CompactAccuracyOf<T>>>,
	{
		const NEEDS_ELECT_DATA: bool = false;
		type Error = Error;
		fn elect<P: PerThing128>(
			_to_elect: usize,
			_targets: Vec<T::AccountId>,
			_voters: Vec<(T::AccountId, VoteWeight, Vec<T::AccountId>)>,
		) -> Result<Supports<T::AccountId>, Self::Error>
		where
			ExtendedBalance: From<<P as PerThing>::Inner>,
		{
			Self::queued_solution()
				.map_or_else(
					|| {
						Self::onchain_fallback()
							.map(|r| (r, ElectionCompute::OnChain))
							.map_err(Into::into)
					},
					|ReadySolution {
					     supports, compute, ..
					 }| Ok((supports, compute)),
				)
				.map(|(supports, compute)| {
					<CurrentPhase<T>>::put(Phase::Off);
					<SnapshotVoters<T>>::kill();
					<SnapshotTargets<T>>::kill();
					Self::deposit_event(RawEvent::ElectionFinalized(Some(compute)));
					{
						let lvl = ::log::Level::Info;
						if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
							::log::__private_api_log(
								::core::fmt::Arguments::new_v1(
									&["\u{1f3e6} Finalized election round with compute ", "."],
									&match (&compute,) {
										(arg0,) => [::core::fmt::ArgumentV1::new(
											arg0,
											::core::fmt::Debug::fmt,
										)],
									},
								),
								lvl,
								&(
									crate::LOG_TARGET,
									"frame_election_providers::two_phase",
									"frame/election-providers/src/two_phase/mod.rs",
									731u32,
								),
							);
						}
					};
					supports
				})
				.map_err(|err| {
					Self::deposit_event(RawEvent::ElectionFinalized(None));
					{
						let lvl = ::log::Level::Error;
						if lvl <= ::log::STATIC_MAX_LEVEL && lvl <= ::log::max_level() {
							::log::__private_api_log(
								::core::fmt::Arguments::new_v1(
									&["\u{1f3e6} Failed to finalize election round. Error = "],
									&match (&err,) {
										(arg0,) => [::core::fmt::ArgumentV1::new(
											arg0,
											::core::fmt::Debug::fmt,
										)],
									},
								),
								lvl,
								&(
									crate::LOG_TARGET,
									"frame_election_providers::two_phase",
									"frame/election-providers/src/two_phase/mod.rs",
									736u32,
								),
							);
						}
					};
					err
				})
		}
		fn ongoing() -> bool {
			match Self::current_phase() {
				Phase::Signed | Phase::Unsigned(_) => true,
				_ => false,
			}
		}
	}
	#[cfg(test)]
	mod tests {
		use super::{mock::*, *};
		use sp_election_providers::ElectionProvider;
		use sp_npos_elections::Support;
		extern crate test;
		#[cfg(test)]
		#[rustc_test_marker]
		pub const phase_rotation_works: test::TestDescAndFn = test::TestDescAndFn {
			desc: test::TestDesc {
				name: test::StaticTestName("two_phase::tests::phase_rotation_works"),
				ignore: false,
				allow_fail: false,
				should_panic: test::ShouldPanic::No,
				test_type: test::TestType::UnitTest,
			},
			testfn: test::StaticTestFn(|| test::assert_test_result(phase_rotation_works())),
		};
		fn phase_rotation_works() {
			ExtBuilder::default().build_and_execute(|| {
				{
					match (&System::block_number(), &0) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				{
					match (&TwoPhase::current_phase(), &Phase::Off) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				{
					match (&TwoPhase::round(), &0) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				roll_to(4);
				{
					match (&TwoPhase::current_phase(), &Phase::Off) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				if !TwoPhase::snapshot_voters().is_none() {
					{
						::std::rt::begin_panic(
							"assertion failed: TwoPhase::snapshot_voters().is_none()",
						)
					}
				};
				{
					match (&TwoPhase::round(), &0) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				roll_to(5);
				{
					match (&TwoPhase::current_phase(), &Phase::Signed) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				if !TwoPhase::snapshot_voters().is_some() {
					{
						::std::rt::begin_panic(
							"assertion failed: TwoPhase::snapshot_voters().is_some()",
						)
					}
				};
				{
					match (&TwoPhase::round(), &1) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				roll_to(14);
				{
					match (&TwoPhase::current_phase(), &Phase::Signed) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				if !TwoPhase::snapshot_voters().is_some() {
					{
						::std::rt::begin_panic(
							"assertion failed: TwoPhase::snapshot_voters().is_some()",
						)
					}
				};
				{
					match (&TwoPhase::round(), &1) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				roll_to(15);
				{
					match (&TwoPhase::current_phase(), &Phase::Unsigned((true, 15))) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				if !TwoPhase::snapshot_voters().is_some() {
					{
						::std::rt::begin_panic(
							"assertion failed: TwoPhase::snapshot_voters().is_some()",
						)
					}
				};
				roll_to(19);
				{
					match (&TwoPhase::current_phase(), &Phase::Unsigned((true, 15))) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				if !TwoPhase::snapshot_voters().is_some() {
					{
						::std::rt::begin_panic(
							"assertion failed: TwoPhase::snapshot_voters().is_some()",
						)
					}
				};
				roll_to(20);
				{
					match (&TwoPhase::current_phase(), &Phase::Unsigned((true, 15))) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				if !TwoPhase::snapshot_voters().is_some() {
					{
						::std::rt::begin_panic(
							"assertion failed: TwoPhase::snapshot_voters().is_some()",
						)
					}
				};
				roll_to(21);
				{
					match (&TwoPhase::current_phase(), &Phase::Unsigned((true, 15))) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				if !TwoPhase::snapshot_voters().is_some() {
					{
						::std::rt::begin_panic(
							"assertion failed: TwoPhase::snapshot_voters().is_some()",
						)
					}
				};
				TwoPhase::elect::<sp_runtime::Perbill>(2, Default::default(), Default::default())
					.unwrap();
				{
					match (&TwoPhase::current_phase(), &Phase::Off) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				if !TwoPhase::snapshot_voters().is_none() {
					{
						::std::rt::begin_panic(
							"assertion failed: TwoPhase::snapshot_voters().is_none()",
						)
					}
				};
				{
					match (&TwoPhase::round(), &1) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
			})
		}
		extern crate test;
		#[cfg(test)]
		#[rustc_test_marker]
		pub const onchain_backup_works: test::TestDescAndFn = test::TestDescAndFn {
			desc: test::TestDesc {
				name: test::StaticTestName("two_phase::tests::onchain_backup_works"),
				ignore: false,
				allow_fail: false,
				should_panic: test::ShouldPanic::No,
				test_type: test::TestType::UnitTest,
			},
			testfn: test::StaticTestFn(|| test::assert_test_result(onchain_backup_works())),
		};
		fn onchain_backup_works() {
			ExtBuilder::default().build_and_execute(|| {
				roll_to(5);
				{
					match (&TwoPhase::current_phase(), &Phase::Signed) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				roll_to(20);
				{
					match (&TwoPhase::current_phase(), &Phase::Unsigned((true, 15))) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				};
				let supports = TwoPhase::elect::<sp_runtime::Perbill>(
					2,
					Default::default(),
					Default::default(),
				)
				.unwrap();
				{
					match (
						&supports,
						&<[_]>::into_vec(box [
							(
								30,
								Support {
									total: 40,
									voters: <[_]>::into_vec(box [(2, 5), (4, 5), (30, 30)]),
								},
							),
							(
								40,
								Support {
									total: 60,
									voters: <[_]>::into_vec(box [
										(2, 5),
										(3, 10),
										(4, 5),
										(40, 40),
									]),
								},
							),
						]),
					) {
						(left_val, right_val) => {
							if !(*left_val == *right_val) {
								{
									::std::rt::begin_panic_fmt(&::core::fmt::Arguments::new_v1(
										&[
											"assertion failed: `(left == right)`\n  left: `",
											"`,\n right: `",
											"`",
										],
										&match (&&*left_val, &&*right_val) {
											(arg0, arg1) => [
												::core::fmt::ArgumentV1::new(
													arg0,
													::core::fmt::Debug::fmt,
												),
												::core::fmt::ArgumentV1::new(
													arg1,
													::core::fmt::Debug::fmt,
												),
											],
										},
									))
								}
							}
						}
					}
				}
			})
		}
	}
}
const LOG_TARGET: &'static str = "election-provider";
#[doc(hidden)]
pub use sp_npos_elections::VoteWeight;
#[doc(hidden)]
pub use sp_runtime::traits::UniqueSaturatedInto;
#[doc(hidden)]
pub use sp_std::convert::TryInto;
#[main]
pub fn main() -> () {
	extern crate test;
	test::test_main_static(&[
		&test_benchmarks,
		&cannot_submit_too_early,
		&should_pay_deposit,
		&good_solution_is_rewarded,
		&bad_solution_is_slashed,
		&suppressed_solution_gets_bond_back,
		&queue_is_always_sorted,
		&cannot_submit_worse_with_full_queue,
		&weakest_is_removed_if_better_provided,
		&equally_good_is_not_accepted,
		&solutions_are_always_sorted,
		&all_in_one_singed_submission_scenario,
		&validate_unsigned_retracts_wrong_phase,
		&validate_unsigned_retracts_low_score,
		&priority_is_set,
		&invalid_solution_panics,
		&miner_works,
		&ocw_will_only_submit_if_feasible,
		&can_only_submit_threshold_better,
		&ocw_check_prevent_duplicate,
		&ocw_only_runs_when_signed_open_now,
		&ocw_can_submit_to_pool,
		&phase_rotation_works,
		&onchain_backup_works,
	])
}