use std::time::Duration;

use am_agents::LocalModelRuntime;
use am_proto::{
    ComputeProviderKind, ComputeQuoteRequest, ComputeQuoteResponse, ComputeSetupStatus,
    ModelTargetKind, OpenModelSpec, RentedComputePolicy, RentedEndpoint, WalletBalance,
};
use keyring::Entry;

use crate::{AppCore, CoreError};

const RENTED_COMPUTE_POLICY_KEY: &str = "rented_compute_policy";
const KEYRING_SERVICE: &str = "com.agentmanager.app";
const KEYRING_CONTROL_PLANE_TOKEN: &str = "rented-compute-control-plane-token";
const KEYRING_LEASE_TOKEN_PREFIX: &str = "rented-compute-lease-token:";
const CONTROL_PLANE_TIMEOUT: Duration = Duration::from_secs(12);

impl AppCore {
    pub async fn get_rented_compute_policy(&self) -> Result<RentedComputePolicy, CoreError> {
        let mut policy = am_db::repos::settings::get(&self.db.pool, RENTED_COMPUTE_POLICY_KEY)
            .await?
            .and_then(|raw| serde_json::from_str::<RentedComputePolicy>(&raw).ok())
            .unwrap_or_default();
        policy.control_plane_token_configured = load_control_plane_token()?.is_some();
        Ok(policy)
    }

    pub async fn set_rented_compute_policy(
        &self,
        mut policy: RentedComputePolicy,
        control_plane_token: Option<String>,
    ) -> Result<RentedComputePolicy, CoreError> {
        policy.spend.auto_purchase_enabled = false;
        if let Some(token) = control_plane_token.as_deref().map(str::trim) {
            if token.is_empty() {
                delete_control_plane_token()?;
            } else {
                store_control_plane_token(token)?;
            }
        }

        policy.control_plane_token_configured = load_control_plane_token()?.is_some();
        let mut stored = policy.clone();
        stored.control_plane_token_configured = false;
        let value = serde_json::to_string(&stored).map_err(|e| CoreError::Other(e.to_string()))?;
        am_db::repos::settings::set(&self.db.pool, RENTED_COMPUTE_POLICY_KEY, &value).await?;
        Ok(policy)
    }

    pub async fn rented_compute_setup_status(&self) -> Result<ComputeSetupStatus, CoreError> {
        let policy = self.get_rented_compute_policy().await?;
        let mut warnings = Vec::new();
        if policy.spend.auto_purchase_enabled && policy.spend.tos_accepted_at.is_none() {
            warnings.push(
                "Auto-purchase is disabled until the rented-compute terms are accepted.".into(),
            );
        }
        if policy.spend.auto_purchase_enabled && policy.spend.max_per_run_usd <= 0.0 {
            warnings.push("Auto-purchase requires a positive per-run cap.".into());
        }
        warnings.push(
            "Quote browsing is enabled. Purchasing is disabled until billing is wired.".into(),
        );
        let wallet_balance =
            if policy.control_plane_url.is_some() && policy.control_plane_token_configured {
                self.fetch_wallet_balance(&policy).await.ok()
            } else {
                None
            };
        Ok(ComputeSetupStatus {
            control_plane_configured: policy.control_plane_url.is_some(),
            authenticated: policy.control_plane_token_configured,
            wallet_balance,
            production_providers: vec![ComputeProviderKind::Vast],
            hidden_providers: vec![ComputeProviderKind::Runpod, ComputeProviderKind::Lambda],
            warnings,
        })
    }

    pub async fn list_open_models(&self) -> Result<Vec<OpenModelSpec>, CoreError> {
        Ok(am_compute::default_model_catalog())
    }

    pub async fn rented_compute_quotes(
        &self,
        request: ComputeQuoteRequest,
    ) -> Result<ComputeQuoteResponse, CoreError> {
        let policy = self.get_rented_compute_policy().await?;
        if !policy.spend.enabled {
            return Err(CoreError::Other(
                "rented compute is disabled in settings".into(),
            ));
        }
        let base = policy
            .control_plane_url
            .as_deref()
            .ok_or_else(|| CoreError::Other("control plane URL is not configured".into()))?;
        let token = load_control_plane_token()?
            .ok_or_else(|| CoreError::Other("control plane token is not configured".into()))?;
        let url = format!("{}/v1/compute/quotes", base.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(CONTROL_PLANE_TIMEOUT)
            .build()
            .map_err(|err| CoreError::Other(err.to_string()))?;
        let response = client
            .post(url)
            .bearer_auth(token)
            .json(&request)
            .send()
            .await
            .map_err(|err| CoreError::Other(err.to_string()))?;
        if !response.status().is_success() {
            return Err(CoreError::Other(format!(
                "control plane quote request returned HTTP {}",
                response.status()
            )));
        }
        response
            .json::<ComputeQuoteResponse>()
            .await
            .map_err(|err| CoreError::Other(err.to_string()))
    }

    pub(crate) fn rented_model_runtime(
        &self,
        lease_id: Option<&str>,
        provider: Option<ComputeProviderKind>,
        model: Option<String>,
        endpoint_base_url: Option<String>,
    ) -> Result<Option<LocalModelRuntime>, CoreError> {
        let Some(lease_id) = lease_id else {
            return Err(CoreError::Other(
                "rented compute target requires a compute lease id".into(),
            ));
        };
        let provider = provider.unwrap_or(ComputeProviderKind::Vast);
        let model = model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| CoreError::Other("rented compute target requires a model".into()))?;
        let endpoint_base_url = endpoint_base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CoreError::Other(
                    "rented compute lease is not ready; no scoped endpoint is configured".into(),
                )
            })?;
        let api_token = load_lease_token(lease_id)?.ok_or_else(|| {
            CoreError::Other(
                "rented compute lease token is missing; reconnect to the control plane".into(),
            )
        })?;
        tracing::debug!(
            lease_id,
            provider = provider.as_str(),
            "using rented model endpoint"
        );
        Ok(Some(LocalModelRuntime {
            provider: am_proto::LocalModelProviderKind::LmStudio,
            model: model.to_string(),
            base_url: Some(endpoint_base_url.to_string()),
            api_token: Some(api_token),
        }))
    }

    pub async fn store_rented_endpoint(&self, endpoint: RentedEndpoint) -> Result<(), CoreError> {
        store_lease_token(&endpoint.lease_id, &endpoint.bearer_token)?;
        let _ = endpoint;
        Ok(())
    }

    pub(crate) async fn enforce_rented_spend_policy(
        &self,
        model_id: &str,
        provider: Option<ComputeProviderKind>,
        max_compute_usd: Option<f64>,
        allow_auto_purchase: bool,
    ) -> Result<(), CoreError> {
        let policy = self.get_rented_compute_policy().await?;
        if !policy.spend.enabled {
            return Err(CoreError::Other(
                "rented compute is disabled in settings".into(),
            ));
        }
        let provider = provider.unwrap_or(ComputeProviderKind::Vast);
        if !policy.spend.provider_allowlist.contains(&provider) {
            return Err(CoreError::Other(format!(
                "{} is not allowed by the rented compute provider allowlist",
                provider.label()
            )));
        }
        if !policy.spend.model_allowlist.is_empty()
            && !policy
                .spend
                .model_allowlist
                .iter()
                .any(|allowed| allowed == model_id)
        {
            return Err(CoreError::Other(
                "model is not allowed by the rented compute model allowlist".into(),
            ));
        }
        if allow_auto_purchase {
            if !policy.spend.auto_purchase_enabled || policy.spend.tos_accepted_at.is_none() {
                return Err(CoreError::Other(
                    "auto-purchase requires explicit enablement and terms acceptance".into(),
                ));
            }
            let requested_cap = max_compute_usd.unwrap_or(policy.spend.max_per_run_usd);
            if requested_cap > policy.spend.max_per_run_usd {
                return Err(CoreError::Other(format!(
                    "requested compute cap ${requested_cap:.2} exceeds the per-run cap ${:.2}",
                    policy.spend.max_per_run_usd
                )));
            }
        }
        Ok(())
    }

    async fn fetch_wallet_balance(
        &self,
        policy: &RentedComputePolicy,
    ) -> Result<WalletBalance, CoreError> {
        let base = policy
            .control_plane_url
            .as_deref()
            .ok_or_else(|| CoreError::Other("control plane URL is not configured".into()))?;
        let token = load_control_plane_token()?
            .ok_or_else(|| CoreError::Other("control plane token is not configured".into()))?;
        let url = format!("{}/v1/wallet", base.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(CONTROL_PLANE_TIMEOUT)
            .build()
            .map_err(|err| CoreError::Other(err.to_string()))?;
        let response = client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|err| CoreError::Other(err.to_string()))?;
        if !response.status().is_success() {
            return Err(CoreError::Other(format!(
                "control plane wallet probe returned HTTP {}",
                response.status()
            )));
        }
        response
            .json::<WalletBalance>()
            .await
            .map_err(|err| CoreError::Other(err.to_string()))
    }
}

fn store_control_plane_token(token: &str) -> Result<(), CoreError> {
    Entry::new(KEYRING_SERVICE, KEYRING_CONTROL_PLANE_TOKEN)
        .map_err(keyring_err)?
        .set_password(token)
        .map_err(keyring_err)
}

fn load_control_plane_token() -> Result<Option<String>, CoreError> {
    match Entry::new(KEYRING_SERVICE, KEYRING_CONTROL_PLANE_TOKEN)
        .map_err(keyring_err)?
        .get_password()
    {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(keyring_err(err)),
    }
}

fn delete_control_plane_token() -> Result<(), CoreError> {
    match Entry::new(KEYRING_SERVICE, KEYRING_CONTROL_PLANE_TOKEN)
        .map_err(keyring_err)?
        .delete_credential()
    {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(keyring_err(err)),
    }
}

fn store_lease_token(lease_id: &str, token: &str) -> Result<(), CoreError> {
    let key = lease_token_key(lease_id);
    Entry::new(KEYRING_SERVICE, &key)
        .map_err(keyring_err)?
        .set_password(token)
        .map_err(keyring_err)
}

fn load_lease_token(lease_id: &str) -> Result<Option<String>, CoreError> {
    let key = lease_token_key(lease_id);
    match Entry::new(KEYRING_SERVICE, &key)
        .map_err(keyring_err)?
        .get_password()
    {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(keyring_err(err)),
    }
}

fn lease_token_key(lease_id: &str) -> String {
    format!("{KEYRING_LEASE_TOKEN_PREFIX}{lease_id}")
}

fn keyring_err(err: keyring::Error) -> CoreError {
    CoreError::Other(format!("keyring error: {err}"))
}

pub(crate) fn normalize_model_target(
    target: ModelTargetKind,
    local_provider: Option<am_proto::LocalModelProviderKind>,
) -> ModelTargetKind {
    if target == ModelTargetKind::FrontierDefault && local_provider.is_some() {
        ModelTargetKind::LocalProvider
    } else {
        target
    }
}
