use crate::eco::device::EcoDevice;
use crate::eco::errors::{EcoError, EcoResult};
use crate::eco::storage::EcoStorage;

pub struct EcoPermissions {
    storage: std::sync::Mutex<EcoStorage>,
}

impl EcoPermissions {
    pub fn new(storage: EcoStorage) -> Self {
        Self {
            storage: std::sync::Mutex::new(storage),
        }
    }

    pub fn is_trusted(&self, device_id: &str) -> bool {
        self.storage.lock().unwrap().is_device_trusted(device_id)
    }

    pub fn trust_device(&self, device: &EcoDevice) -> EcoResult<()> {
        let id = device.id.to_string();
        self.storage.lock().unwrap().trust_device(&id)
    }

    pub fn untrust_device(&self, device_id: &str) -> EcoResult<()> {
        self.storage.lock().unwrap().untrust_device(device_id)
    }

    pub fn require_trusted(&self, device_id: &str) -> EcoResult<()> {
        if !self.is_trusted(device_id) {
            return Err(EcoError::NotTrusted);
        }
        Ok(())
    }

    pub fn get_trusted_ids(&self) -> Vec<String> {
        self.storage.lock().unwrap().get_trusted_device_ids().clone()
    }
}
