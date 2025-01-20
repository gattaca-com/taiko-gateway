use alloy_primitives::{PrimitiveSignature, B256};
use alloy_signer::k256::{NonZeroScalar, Scalar, Secp256k1};
use ecdsa::hazmat::{bits2field, sign_prehashed};
use eyre::eyre;

/// ECDSA normally signs over a randomized `k` but Taiko needs anchor transactions to be rebuilt
/// determinstically by all clients
/// https://github.com/taikoxyz/taiko-mono/blob/ontake_preconfs/packages/taiko-client/driver/signer/fixed_k_signer.go
pub fn sign_fixed_k(prehash: B256, scalar: &NonZeroScalar) -> eyre::Result<PrimitiveSignature> {
    const K_1: Scalar = Scalar::ONE;
    const K_2: Scalar = K_1.add(&K_1);

    let z = bits2field::<Secp256k1>(prehash.as_ref())?;

    // first try K=1
    let sig_1 = sign_prehashed::<Secp256k1, Scalar>(scalar, K_1, &z);
    let (recoverable_sig, recovery_id) = match sig_1 {
        Ok(res) => res,
        // then try K=2
        Err(_) => sign_prehashed::<Secp256k1, Scalar>(scalar, K_2, &z)
            .map_err(|_| eyre!("failed to sign anchor using K=1 and K=2"))?,
    };

    let sig =
        PrimitiveSignature::from_signature_and_parity(recoverable_sig, recovery_id.is_y_odd());
    let normalized = sig.normalize_s().unwrap_or(sig);

    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{b256, PrimitiveSignature};
    use alloy_signer_local::PrivateKeySigner;

    use super::*;
    use crate::taiko::{GOLDEN_TOUCH_ADDRESS, GOLDEN_TOUCH_PRIVATE_KEY};

    #[test]
    fn test_sign() {
        let private = PrivateKeySigner::from_bytes(&GOLDEN_TOUCH_PRIVATE_KEY).unwrap();
        let scalar = private.as_nonzero_scalar();

        let prehash = b256!("44943399d1507f3ce7525e9be2f987c3db9136dc759cb7f92f742154196868b9");

        let signature = PrimitiveSignature::from_scalars_and_parity(
            b256!("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"),
            b256!("782a1e70872ecc1a9f740dd445664543f8b7598c94582720bca9a8c48d6a4766"),
            true,
        );

        let address = signature.recover_address_from_prehash(&prehash).expect("recover");
        assert_eq!(address, GOLDEN_TOUCH_ADDRESS);

        let test_sig = sign_fixed_k(prehash, scalar).expect("sign");
        assert_eq!(signature, test_sig)
    }

    #[test]
    fn test_sign_2() {
        let private = PrivateKeySigner::from_bytes(&GOLDEN_TOUCH_PRIVATE_KEY).unwrap();
        let scalar = private.as_nonzero_scalar();

        let prehash = b256!("663d210fa6dba171546498489de1ba024b89db49e21662f91bf83cdffe788820");

        let signature = PrimitiveSignature::from_scalars_and_parity(
            b256!("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"),
            b256!("568130fab1a3a9e63261d4278a7e130588beb51f27de7c20d0258d38a85a27ff"),
            true,
        );

        let address = signature.recover_address_from_prehash(&prehash).expect("recover");
        assert_eq!(address, GOLDEN_TOUCH_ADDRESS);

        let test_sig = sign_fixed_k(prehash, scalar).expect("sign");
        assert_eq!(signature, test_sig)
    }
}
