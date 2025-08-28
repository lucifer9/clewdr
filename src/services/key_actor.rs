use std::collections::{HashSet, VecDeque};

use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use serde::Serialize;
use snafu::{GenerateImplicitData, Location};
use tracing::{error, info};

use crate::{
    config::{CLEWDR_CONFIG, ClewdrConfig, KeyStatus},
    error::ClewdrError,
};

#[derive(Debug, Serialize, Clone)]
pub struct KeyStatusInfo {
    pub valid: Vec<KeyStatus>,
}

/// Messages that the KeyActor can handle
#[derive(Debug)]
enum KeyActorMessage {
    /// Return a Key
    Return(KeyStatus),
    /// Submit a new Key
    Submit(KeyStatus),
    /// Request to get a Key
    Request(RpcReplyPort<Result<KeyStatus, ClewdrError>>),
    /// Get all Key status information
    GetStatus(RpcReplyPort<KeyStatusInfo>),
    /// Delete a Key
    Delete(KeyStatus, RpcReplyPort<Result<(), ClewdrError>>),
}

/// KeyActor state - manages the collection of valid keys
type KeyActorState = VecDeque<KeyStatus>;

/// Key actor that handles key distribution and status tracking using Ractor
struct KeyActor;

impl KeyActor {
    /// Saves the current state of keys to the configuration
    fn save(state: &KeyActorState) {
        info!("[KEY_ACTOR] Updating configuration with {} keys", state.len());
        CLEWDR_CONFIG.rcu(|config| {
            let mut config = ClewdrConfig::clone(config);
            config.gemini_keys = state.iter().cloned().collect();
            config
        });

        tokio::spawn(async move {
            info!("[KEY_ACTOR] Starting configuration file save...");
            let result = CLEWDR_CONFIG.load().save().await;
            match result {
                Ok(_) => info!("[KEY_ACTOR] Configuration saved successfully to file"),
                Err(e) => error!("[KEY_ACTOR] Failed to save configuration to file: {}", e),
            }
        });
    }

    /// Dispatches a key for use
    fn dispatch(state: &mut KeyActorState) -> Result<KeyStatus, ClewdrError> {
        // 找到第一个可用的密钥（不在冷却中）
        let available_index = state
            .iter()
            .position(|key| key.is_available())
            .ok_or(ClewdrError::NoKeyAvailable)?;
        
        // 移除可用的密钥并放到队列末尾
        let key = state.remove(available_index).unwrap();
        state.push_back(key.clone());
        Ok(key)
    }

    /// Collects (returns) a key back to the pool
    fn collect(state: &mut KeyActorState, key: KeyStatus) {
        let Some(pos) = state.iter().position(|k| k.key == key.key) else {
            error!("[KEY_ACTOR] Key not found in valid keys: {}", key.key.ellipse());
            return;
        };
        
        let old_cooldown = state[pos].cooldown_until;
        let cooldown_changed = old_cooldown != key.cooldown_until;
        
        info!(
            "[KEY_ACTOR] Updating key {}: cooldown {:?} -> {:?}",
            key.key.ellipse(), old_cooldown, key.cooldown_until
        );
        
        // 更新状态
        state[pos] = key;
        
        // 如果cooldown状态变化，保存配置
        if cooldown_changed {
            info!("[KEY_ACTOR] Cooldown changed, saving configuration");
            Self::save(state);
        } else {
            info!("[KEY_ACTOR] No cooldown change, skipping save");
        }
    }

    /// Accepts a new key into the valid collection
    fn accept(state: &mut KeyActorState, key: KeyStatus) {
        if CLEWDR_CONFIG.load().gemini_keys.contains(&key) {
            info!("Key already exists");
            return;
        }
        state.push_back(key);
        Self::save(state);
    }

    /// Creates a report of all key statuses
    fn report(state: &KeyActorState) -> KeyStatusInfo {
        KeyStatusInfo {
            valid: state.iter().cloned().collect(),
        }
    }

    /// Deletes a key from the collection
    fn delete(state: &mut KeyActorState, key: KeyStatus) -> Result<(), ClewdrError> {
        let size_before = state.len();
        info!("[KEY_ACTOR] Attempting to delete key: {}", key.key.ellipse());
        state.retain(|k| *k != key);

        if state.len() < size_before {
            info!("[KEY_ACTOR] Key deleted successfully, {} keys remaining", state.len());
            Self::save(state);
            Ok(())
        } else {
            error!("[KEY_ACTOR] Delete operation failed - key not found: {}", key.key.ellipse());
            Err(ClewdrError::UnexpectedNone {
                msg: "Delete operation did not find the key",
            })
        }
    }
}

impl Actor for KeyActor {
    type Msg = KeyActorMessage;
    type State = KeyActorState;
    type Arguments = HashSet<KeyStatus>;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let state: Self::State = VecDeque::from_iter(args);
        Ok(state)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            KeyActorMessage::Return(key) => {
                Self::collect(state, key);
            }
            KeyActorMessage::Submit(key) => {
                Self::accept(state, key);
            }
            KeyActorMessage::Request(reply_port) => {
                let result = Self::dispatch(state);
                reply_port.send(result)?;
            }
            KeyActorMessage::GetStatus(reply_port) => {
                let status_info = Self::report(state);
                reply_port.send(status_info)?;
            }
            KeyActorMessage::Delete(key, reply_port) => {
                let result = Self::delete(state, key);
                reply_port.send(result)?;
            }
        }
        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        KeyActor::save(state);
        Ok(())
    }
}

/// Handle for interacting with the KeyActor
#[derive(Clone)]
pub struct KeyActorHandle {
    actor_ref: ActorRef<KeyActorMessage>,
}

impl KeyActorHandle {
    /// Create a new KeyActor and return a handle to it
    pub async fn start() -> Result<Self, ractor::SpawnErr> {
        let (actor_ref, _join_handle) =
            Actor::spawn(None, KeyActor, CLEWDR_CONFIG.load().gemini_keys.clone()).await?;
        Ok(Self { actor_ref })
    }

    /// Request a key from the key actor
    pub async fn request(&self) -> Result<KeyStatus, ClewdrError> {
        ractor::call!(self.actor_ref, KeyActorMessage::Request).map_err(|e| {
            ClewdrError::RactorError {
                loc: Location::generate(),
                msg: format!("Failed to communicate with KeyActor for request operation: {e}"),
            }
        })?
    }

    /// Return a key to the key actor
    pub async fn return_key(&self, key: KeyStatus) -> Result<(), ClewdrError> {
        ractor::cast!(self.actor_ref, KeyActorMessage::Return(key)).map_err(|e| {
            ClewdrError::RactorError {
                loc: Location::generate(),
                msg: format!("Failed to communicate with KeyActor for return operation: {e}"),
            }
        })
    }

    /// Submit a new key to the key actor
    pub async fn submit(&self, key: KeyStatus) -> Result<(), ClewdrError> {
        ractor::cast!(self.actor_ref, KeyActorMessage::Submit(key)).map_err(|e| {
            ClewdrError::RactorError {
                loc: Location::generate(),
                msg: format!("Failed to communicate with KeyActor for submit operation: {e}"),
            }
        })
    }

    /// Get status information about all keys
    pub async fn get_status(&self) -> Result<KeyStatusInfo, ClewdrError> {
        ractor::call!(self.actor_ref, KeyActorMessage::GetStatus).map_err(|e| {
            ClewdrError::RactorError {
                loc: Location::generate(),
                msg: format!("Failed to communicate with KeyActor for get status operation: {e}"),
            }
        })
    }

    /// Delete a key from the key actor
    pub async fn delete_key(&self, key: KeyStatus) -> Result<(), ClewdrError> {
        ractor::call!(self.actor_ref, KeyActorMessage::Delete, key).map_err(|e| {
            ClewdrError::RactorError {
                loc: Location::generate(),
                msg: format!("Failed to communicate with KeyActor for delete operation: {e}"),
            }
        })?
    }
}
