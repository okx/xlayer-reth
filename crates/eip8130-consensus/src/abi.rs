//! `sol!` ABI definitions for EIP-8130 system contracts and verifiers.

use alloy_sol_types::sol;

sol! {
    /// Verifier interface.
    interface IVerifier {
        function verify(bytes32 hash, bytes calldata data) external view returns (bytes32 ownerId);
    }
}

sol! {
    /// Owner tuple used in account creation.
    struct OwnerTuple {
        address verifier;
        bytes32 ownerId;
        uint8 scope;
    }

    /// Config operation tuple used in config changes.
    struct ConfigOpTuple {
        uint8 changeType;
        address verifier;
        bytes32 ownerId;
        uint8 scope;
    }

    /// Account configuration system contract.
    interface IAccountConfig {
        function getOwner(address account, bytes32 ownerId) external view returns (address verifier, uint8 scope);
        function createAccount(bytes32 userSalt, bytes calldata bytecode, OwnerTuple[] calldata initialOwners) external returns (address);
        function getAddress(bytes32 userSalt, bytes calldata bytecode, OwnerTuple[] calldata initialOwners) external view returns (address);
        function applyConfigChange(address account, uint64 chainId, uint64 sequence, ConfigOpTuple[] calldata ownerChanges, bytes calldata authorizerAuth) external;
        function getChangeSequence(address account, uint64 chainId) external view returns (uint64);
        function lock(address account, uint32 unlockDelay, bytes calldata signature) external;
        function requestUnlock(address account, bytes calldata signature) external;
        function unlock(address account, bytes calldata signature) external;
        function getLockState(address account) external view returns (bool locked, uint32 unlockDelay, uint32 unlockRequestedAt);
        function getNativeVerifiers() external view returns (address k1, address p256Raw, address p256WebAuthn, address delegate);
        function getVerifierAddress(uint8 verifierType) external view returns (address);
        function verifySignature(address account, bytes32 hash, bytes calldata auth) external view returns (bool valid, bytes32 ownerId, address verifier);
    }
}

sol! {
    /// Nonce Manager precompile interface.
    interface INonceManager {
        function getNonce(address account, uint256 nonceKey) external view returns (uint64);
    }
}

sol! {
    /// Call tuple for TxContext return values.
    struct CallTuple {
        address target;
        bytes data;
    }

    /// Transaction context precompile interface.
    interface ITxContext {
        function getSender() external view returns (address);
        function getPayer() external view returns (address);
        function getOwnerId() external view returns (bytes32);
        function getCalls() external view returns (CallTuple[][] memory);
        function getMaxCost() external view returns (uint256);
        function getGasLimit() external view returns (uint256);
    }
}
