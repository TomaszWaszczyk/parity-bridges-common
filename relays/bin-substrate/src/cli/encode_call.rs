// Copyright 2019-2021 Parity Technologies (UK) Ltd.
// This file is part of Parity Bridges Common.

// Parity Bridges Common is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity Bridges Common is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity Bridges Common.  If not, see <http://www.gnu.org/licenses/>.

use crate::cli::bridge::FullBridge;
use crate::cli::{AccountId, Balance, CliChain, ExplicitOrMaximal, HexBytes, HexLaneId};
use crate::select_full_bridge;
use frame_support::weights::DispatchInfo;
use relay_substrate_client::Chain;
use structopt::StructOpt;
use strum::VariantNames;

/// Encode source chain runtime call.
#[derive(StructOpt, Debug)]
pub struct EncodeCall {
	/// A bridge instance to encode call for.
	#[structopt(possible_values = FullBridge::VARIANTS, case_insensitive = true)]
	bridge: FullBridge,
	#[structopt(flatten)]
	call: Call,
}

/// All possible messages that may be delivered to generic Substrate chain.
///
/// Note this enum may be used in the context of both Source (as part of `encode-call`)
/// and Target chain (as part of `encode-message/send-message`).
#[derive(StructOpt, Debug, PartialEq, Eq)]
pub enum Call {
	/// Raw bytes for the message
	Raw {
		/// Raw, SCALE-encoded message
		data: HexBytes,
	},
	/// Make an on-chain remark (comment).
	Remark {
		/// Explicit remark payload.
		#[structopt(long, conflicts_with("remark-size"))]
		remark_payload: Option<HexBytes>,
		/// Remark size. If not passed, small UTF8-encoded string is generated by relay as remark.
		#[structopt(long, conflicts_with("remark-payload"))]
		remark_size: Option<ExplicitOrMaximal<usize>>,
	},
	/// Transfer the specified `amount` of native tokens to a particular `recipient`.
	Transfer {
		/// Address of an account to receive the transfer.
		#[structopt(long)]
		recipient: AccountId,
		/// Amount of target tokens to send in target chain base currency units.
		#[structopt(long)]
		amount: Balance,
	},
	/// A call to the specific Bridge Messages pallet to queue message to be sent over a bridge.
	BridgeSendMessage {
		/// An index of the bridge instance which represents the expected target chain.
		#[structopt(skip = 255)]
		bridge_instance_index: u8,
		/// Hex-encoded lane id that should be served by the relay. Defaults to `00000000`.
		#[structopt(long, default_value = "00000000")]
		lane: HexLaneId,
		/// Raw SCALE-encoded Message Payload to submit to the messages pallet.
		///
		/// This can be obtained by encoding call for the target chain.
		#[structopt(long)]
		payload: HexBytes,
		/// Declared delivery and dispatch fee in base source-chain currency units.
		#[structopt(long)]
		fee: Balance,
	},
}

pub trait CliEncodeCall: Chain {
	/// Maximal size (in bytes) of any extrinsic (from the runtime).
	fn max_extrinsic_size() -> u32;

	/// Encode a CLI call.
	fn encode_call(call: &Call) -> anyhow::Result<Self::Call>;

	/// Get dispatch info for the call.
	fn get_dispatch_info(call: &Self::Call) -> anyhow::Result<DispatchInfo>;
}

impl EncodeCall {
	fn encode(&mut self) -> anyhow::Result<HexBytes> {
		select_full_bridge!(self.bridge, {
			preprocess_call::<Source, Target>(&mut self.call, self.bridge.bridge_instance_index());
			let call = Source::encode_call(&self.call)?;

			let encoded = HexBytes::encode(&call);

			log::info!(target: "bridge", "Generated {} call: {:#?}", Source::NAME, call);
			log::info!(target: "bridge", "Weight of {} call: {}", Source::NAME, Source::get_dispatch_info(&call)?.weight);
			log::info!(target: "bridge", "Encoded {} call: {:?}", Source::NAME, encoded);

			Ok(encoded)
		})
	}

	/// Run the command.
	pub async fn run(mut self) -> anyhow::Result<()> {
		println!("{:?}", self.encode()?);
		Ok(())
	}
}

/// Prepare the call to be passed to [`CliEncodeCall::encode_call`].
///
/// This function will fill in all optional and missing pieces and will make sure that
/// values are converted to bridge-specific ones.
///
/// Most importantly, the method will fill-in [`bridge_instance_index`] parameter for
/// target-chain specific calls.
pub(crate) fn preprocess_call<Source: CliEncodeCall + CliChain, Target: CliEncodeCall>(
	call: &mut Call,
	bridge_instance: u8,
) {
	match *call {
		Call::Raw { .. } => {}
		Call::Remark {
			ref remark_size,
			ref mut remark_payload,
		} => {
			if remark_payload.is_none() {
				*remark_payload = Some(HexBytes(generate_remark_payload(
					remark_size,
					compute_maximal_message_arguments_size(Source::max_extrinsic_size(), Target::max_extrinsic_size()),
				)));
			}
		}
		Call::Transfer { ref mut recipient, .. } => {
			recipient.enforce_chain::<Source>();
		}
		Call::BridgeSendMessage {
			ref mut bridge_instance_index,
			..
		} => {
			*bridge_instance_index = bridge_instance;
		}
	};
}

fn generate_remark_payload(remark_size: &Option<ExplicitOrMaximal<usize>>, maximal_allowed_size: u32) -> Vec<u8> {
	match remark_size {
		Some(ExplicitOrMaximal::Explicit(remark_size)) => vec![0; *remark_size],
		Some(ExplicitOrMaximal::Maximal) => vec![0; maximal_allowed_size as _],
		None => format!(
			"Unix time: {}",
			std::time::SystemTime::now()
				.duration_since(std::time::SystemTime::UNIX_EPOCH)
				.unwrap_or_default()
				.as_secs(),
		)
		.as_bytes()
		.to_vec(),
	}
}

pub(crate) fn compute_maximal_message_arguments_size(
	maximal_source_extrinsic_size: u32,
	maximal_target_extrinsic_size: u32,
) -> u32 {
	// assume that both signed extensions and other arguments fit 1KB
	let service_tx_bytes_on_source_chain = 1024;
	let maximal_source_extrinsic_size = maximal_source_extrinsic_size - service_tx_bytes_on_source_chain;
	let maximal_call_size =
		bridge_runtime_common::messages::target::maximal_incoming_message_size(maximal_target_extrinsic_size);
	let maximal_call_size = if maximal_call_size > maximal_source_extrinsic_size {
		maximal_source_extrinsic_size
	} else {
		maximal_call_size
	};

	// bytes in Call encoding that are used to encode everything except arguments
	let service_bytes = 1 + 1 + 4;
	maximal_call_size - service_bytes
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn should_encode_transfer_call() {
		// given
		let mut encode_call = EncodeCall::from_iter(vec![
			"encode-call",
			"rialto-to-millau",
			"transfer",
			"--amount",
			"12345",
			"--recipient",
			"5sauUXUfPjmwxSgmb3tZ5d6yx24eZX4wWJ2JtVUBaQqFbvEU",
		]);

		// when
		let hex = encode_call.encode().unwrap();

		// then
		assert_eq!(
			format!("{:?}", hex),
			"0x0c00d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27de5c0"
		);
	}

	#[test]
	fn should_encode_remark_with_default_payload() {
		// given
		let mut encode_call = EncodeCall::from_iter(vec!["encode-call", "rialto-to-millau", "remark"]);

		// when
		let hex = encode_call.encode().unwrap();

		// then
		assert!(format!("{:?}", hex).starts_with("0x070154556e69782074696d653a"));
	}

	#[test]
	fn should_encode_remark_with_explicit_payload() {
		// given
		let mut encode_call = EncodeCall::from_iter(vec![
			"encode-call",
			"rialto-to-millau",
			"remark",
			"--remark-payload",
			"1234",
		]);

		// when
		let hex = encode_call.encode().unwrap();

		// then
		assert_eq!(format!("{:?}", hex), "0x0701081234");
	}

	#[test]
	fn should_encode_remark_with_size() {
		// given
		let mut encode_call =
			EncodeCall::from_iter(vec!["encode-call", "rialto-to-millau", "remark", "--remark-size", "12"]);

		// when
		let hex = encode_call.encode().unwrap();

		// then
		assert_eq!(format!("{:?}", hex), "0x070130000000000000000000000000");
	}

	#[test]
	fn should_disallow_both_payload_and_size() {
		// when
		let err = EncodeCall::from_iter_safe(vec![
			"encode-call",
			"rialto-to-millau",
			"remark",
			"--remark-payload",
			"1234",
			"--remark-size",
			"12",
		])
		.unwrap_err();

		// then
		assert_eq!(err.kind, structopt::clap::ErrorKind::ArgumentConflict);

		let info = err.info.unwrap();
		assert!(info.contains(&"remark-payload".to_string()) | info.contains(&"remark-size".to_string()))
	}
}
