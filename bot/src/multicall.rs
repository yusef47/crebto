use alloy::{
    network::Ethereum,
    primitives::{Address, Bytes},
    providers::Provider,
    sol,
    transports::Transport,
};
use eyre::Result;

sol! {
    #[sol(rpc)]
    interface IMulticall3 {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }
        struct Result {
            bool success;
            bytes returnData;
        }
        function aggregate3(Call3[] calldata calls) external payable returns (Result[] memory returnData);
    }
}

/// Aggregate multiple static calls via Multicall3.
/// Uses aggregate3 so individual call failures do not revert the entire batch.
/// Returns a vector of (success, return_data) tuples in the same order as `calls`.
pub async fn multicall3_aggregate3<T, P>(
    provider: &P,
    multicall3: Address,
    calls: Vec<(Address, Bytes)>,
) -> Result<Vec<(bool, Bytes)>>
where
    T: Transport + Clone,
    P: Provider<T, Ethereum>,
{
    if calls.is_empty() {
        return Ok(Vec::new());
    }

    let mc_calls: Vec<IMulticall3::Call3> = calls
        .into_iter()
        .map(|(target, call_data)| IMulticall3::Call3 {
            target,
            allowFailure: true,
            callData: call_data,
        })
        .collect();

    let multicall = IMulticall3::new(multicall3, provider);
    let result = multicall.aggregate3(mc_calls).call().await?;

    Ok(result.returnData.into_iter().map(|r| (r.success, Bytes::from(r.returnData))).collect())
}

/// Convenience wrapper: build a batch of `balanceOf` calls for a single ERC20 token
/// and return balances aligned with the input addresses.
pub fn build_balance_of_calls(token: Address, holders: &[Address]) -> Vec<(Address, Bytes)> {
    use alloy::sol_types::SolCall;

    holders
        .iter()
        .map(|holder| {
            let call = IERC20BalanceOf::balanceOfCall {
                account: *holder,
            };
            (token, Bytes::from(call.abi_encode()))
        })
        .collect()
}

sol! {
    #[derive(Debug)]
    interface IERC20BalanceOf {
        function balanceOf(address account) external view returns (uint256);
    }
}
