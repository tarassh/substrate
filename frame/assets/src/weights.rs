// This file is part of Substrate.

// Copyright (C) 2021 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Autogenerated weights for pallet_assets
//!
//! THIS FILE WAS AUTO-GENERATED USING THE SUBSTRATE BENCHMARK CLI VERSION 3.0.0
//! DATE: 2021-02-28, STEPS: [25, ], REPEAT: 2, LOW RANGE: [], HIGH RANGE: []
//! EXECUTION: Some(Wasm), WASM-EXECUTION: Compiled, CHAIN: Some("dev"), DB CACHE: 128

// Executed Command:
// ../../../target/release/substrate
// benchmark
// --chain
// dev
// --steps
// 25
// --repeat
// 2
// --pallet
// pallet_assets
// --extrinsic
// *
// --raw
// --execution=wasm
// --wasm-execution=compiled
// --output
// ../../../frame/assets/src/weights.rs
// --template
// ../../../.maintain/frame-weight-template.hbs


#![allow(unused_parens)]
#![allow(unused_imports)]

use frame_support::{traits::Get, weights::{Weight, constants::RocksDbWeight}};
use sp_std::marker::PhantomData;

/// Weight functions needed for pallet_assets.
pub trait WeightInfo {
	fn create() -> Weight;
	fn force_create() -> Weight;
	fn destroy(c: u32, s: u32, a: u32, ) -> Weight;
	fn force_destroy(c: u32, s: u32, a: u32, ) -> Weight;
	fn mint() -> Weight;
	fn burn() -> Weight;
	fn transfer() -> Weight;
	fn force_transfer() -> Weight;
	fn freeze() -> Weight;
	fn thaw() -> Weight;
	fn freeze_asset() -> Weight;
	fn thaw_asset() -> Weight;
	fn transfer_ownership() -> Weight;
	fn set_team() -> Weight;
	fn set_metadata(n: u32, s: u32, ) -> Weight;
	fn clear_metadata() -> Weight;
	fn force_set_metadata(n: u32, s: u32, ) -> Weight;
	fn force_clear_metadata() -> Weight;
	fn force_asset_status() -> Weight;
	fn approve_transfer() -> Weight;
	fn transfer_approved() -> Weight;
	fn cancel_approval() -> Weight;
	fn force_cancel_approval() -> Weight;
}

/// Weights for pallet_assets using the Substrate node and recommended hardware.
pub struct SubstrateWeight<T>(PhantomData<T>);
impl<T: frame_system::Config> WeightInfo for SubstrateWeight<T> {
	fn create() -> Weight {
		(71_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(1 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn force_create() -> Weight {
		(36_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(1 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn destroy(c: u32, s: u32, a: u32, ) -> Weight {
		(0 as Weight)
			// Standard Error: 3_237_000
			.saturating_add((38_395_000 as Weight).saturating_mul(c as Weight))
			// Standard Error: 3_237_000
			.saturating_add((46_875_000 as Weight).saturating_mul(s as Weight))
			// Standard Error: 323_000
			.saturating_add((2_010_000 as Weight).saturating_mul(a as Weight))
			.saturating_add(T::DbWeight::get().reads(4 as Weight))
			.saturating_add(T::DbWeight::get().reads((2 as Weight).saturating_mul(c as Weight)))
			.saturating_add(T::DbWeight::get().reads((2 as Weight).saturating_mul(s as Weight)))
			.saturating_add(T::DbWeight::get().writes(2 as Weight))
			.saturating_add(T::DbWeight::get().writes((2 as Weight).saturating_mul(c as Weight)))
			.saturating_add(T::DbWeight::get().writes((2 as Weight).saturating_mul(s as Weight)))
			.saturating_add(T::DbWeight::get().writes((1 as Weight).saturating_mul(a as Weight)))
	}
	fn force_destroy(c: u32, s: u32, a: u32, ) -> Weight {
		(382_873_000 as Weight)
			// Standard Error: 2_069_000
			.saturating_add((25_300_000 as Weight).saturating_mul(c as Weight))
			// Standard Error: 2_069_000
			.saturating_add((38_388_000 as Weight).saturating_mul(s as Weight))
			// Standard Error: 206_000
			.saturating_add((2_145_000 as Weight).saturating_mul(a as Weight))
			.saturating_add(T::DbWeight::get().reads(4 as Weight))
			.saturating_add(T::DbWeight::get().reads((2 as Weight).saturating_mul(c as Weight)))
			.saturating_add(T::DbWeight::get().reads((2 as Weight).saturating_mul(s as Weight)))
			.saturating_add(T::DbWeight::get().writes(2 as Weight))
			.saturating_add(T::DbWeight::get().writes((2 as Weight).saturating_mul(c as Weight)))
			.saturating_add(T::DbWeight::get().writes((2 as Weight).saturating_mul(s as Weight)))
			.saturating_add(T::DbWeight::get().writes((1 as Weight).saturating_mul(a as Weight)))
	}
	fn mint() -> Weight {
		(68_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(2 as Weight))
			.saturating_add(T::DbWeight::get().writes(2 as Weight))
	}
	fn burn() -> Weight {
		(128_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(2 as Weight))
			.saturating_add(T::DbWeight::get().writes(2 as Weight))
	}
	fn transfer() -> Weight {
		(135_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(4 as Weight))
			.saturating_add(T::DbWeight::get().writes(4 as Weight))
	}
	fn force_transfer() -> Weight {
		(100_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(4 as Weight))
			.saturating_add(T::DbWeight::get().writes(4 as Weight))
	}
	fn freeze() -> Weight {
		(101_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(2 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn thaw() -> Weight {
		(48_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(2 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn freeze_asset() -> Weight {
		(35_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(1 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn thaw_asset() -> Weight {
		(34_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(1 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn transfer_ownership() -> Weight {
		(41_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(2 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn set_team() -> Weight {
		(35_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(1 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn set_metadata(_n: u32, _s: u32, ) -> Weight {
		(79_036_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(2 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn clear_metadata() -> Weight {
		(73_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(2 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn force_set_metadata(_n: u32, s: u32, ) -> Weight {
		(39_978_000 as Weight)
			// Standard Error: 42_000
			.saturating_add((15_000 as Weight).saturating_mul(s as Weight))
			.saturating_add(T::DbWeight::get().reads(2 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn force_clear_metadata() -> Weight {
		(71_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(2 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn force_asset_status() -> Weight {
		(34_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(1 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn approve_transfer() -> Weight {
		(84_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(1 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn transfer_approved() -> Weight {
		(130_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(5 as Weight))
			.saturating_add(T::DbWeight::get().writes(5 as Weight))
	}
	fn cancel_approval() -> Weight {
		(67_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(1 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
	fn force_cancel_approval() -> Weight {
		(75_000_000 as Weight)
			.saturating_add(T::DbWeight::get().reads(2 as Weight))
			.saturating_add(T::DbWeight::get().writes(1 as Weight))
	}
}

// For backwards compatibility and tests
impl WeightInfo for () {
	fn create() -> Weight {
		(71_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(1 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn force_create() -> Weight {
		(36_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(1 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn destroy(c: u32, s: u32, a: u32, ) -> Weight {
		(0 as Weight)
			// Standard Error: 3_237_000
			.saturating_add((38_395_000 as Weight).saturating_mul(c as Weight))
			// Standard Error: 3_237_000
			.saturating_add((46_875_000 as Weight).saturating_mul(s as Weight))
			// Standard Error: 323_000
			.saturating_add((2_010_000 as Weight).saturating_mul(a as Weight))
			.saturating_add(RocksDbWeight::get().reads(4 as Weight))
			.saturating_add(RocksDbWeight::get().reads((2 as Weight).saturating_mul(c as Weight)))
			.saturating_add(RocksDbWeight::get().reads((2 as Weight).saturating_mul(s as Weight)))
			.saturating_add(RocksDbWeight::get().writes(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes((2 as Weight).saturating_mul(c as Weight)))
			.saturating_add(RocksDbWeight::get().writes((2 as Weight).saturating_mul(s as Weight)))
			.saturating_add(RocksDbWeight::get().writes((1 as Weight).saturating_mul(a as Weight)))
	}
	fn force_destroy(c: u32, s: u32, a: u32, ) -> Weight {
		(382_873_000 as Weight)
			// Standard Error: 2_069_000
			.saturating_add((25_300_000 as Weight).saturating_mul(c as Weight))
			// Standard Error: 2_069_000
			.saturating_add((38_388_000 as Weight).saturating_mul(s as Weight))
			// Standard Error: 206_000
			.saturating_add((2_145_000 as Weight).saturating_mul(a as Weight))
			.saturating_add(RocksDbWeight::get().reads(4 as Weight))
			.saturating_add(RocksDbWeight::get().reads((2 as Weight).saturating_mul(c as Weight)))
			.saturating_add(RocksDbWeight::get().reads((2 as Weight).saturating_mul(s as Weight)))
			.saturating_add(RocksDbWeight::get().writes(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes((2 as Weight).saturating_mul(c as Weight)))
			.saturating_add(RocksDbWeight::get().writes((2 as Weight).saturating_mul(s as Weight)))
			.saturating_add(RocksDbWeight::get().writes((1 as Weight).saturating_mul(a as Weight)))
	}
	fn mint() -> Weight {
		(68_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes(2 as Weight))
	}
	fn burn() -> Weight {
		(128_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes(2 as Weight))
	}
	fn transfer() -> Weight {
		(135_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(4 as Weight))
			.saturating_add(RocksDbWeight::get().writes(4 as Weight))
	}
	fn force_transfer() -> Weight {
		(100_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(4 as Weight))
			.saturating_add(RocksDbWeight::get().writes(4 as Weight))
	}
	fn freeze() -> Weight {
		(101_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn thaw() -> Weight {
		(48_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn freeze_asset() -> Weight {
		(35_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(1 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn thaw_asset() -> Weight {
		(34_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(1 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn transfer_ownership() -> Weight {
		(41_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn set_team() -> Weight {
		(35_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(1 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn set_metadata(_n: u32, _s: u32, ) -> Weight {
		(79_036_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn clear_metadata() -> Weight {
		(73_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn force_set_metadata(_n: u32, s: u32, ) -> Weight {
		(39_978_000 as Weight)
			// Standard Error: 42_000
			.saturating_add((15_000 as Weight).saturating_mul(s as Weight))
			.saturating_add(RocksDbWeight::get().reads(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn force_clear_metadata() -> Weight {
		(71_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn force_asset_status() -> Weight {
		(34_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(1 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn approve_transfer() -> Weight {
		(84_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(1 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn transfer_approved() -> Weight {
		(130_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(5 as Weight))
			.saturating_add(RocksDbWeight::get().writes(5 as Weight))
	}
	fn cancel_approval() -> Weight {
		(67_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(1 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
	fn force_cancel_approval() -> Weight {
		(75_000_000 as Weight)
			.saturating_add(RocksDbWeight::get().reads(2 as Weight))
			.saturating_add(RocksDbWeight::get().writes(1 as Weight))
	}
}
