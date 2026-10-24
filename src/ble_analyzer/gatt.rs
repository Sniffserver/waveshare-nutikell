//! Read-Only GATT Explorer & Trait Adapter for TrouBLE.
//!
//! Enforces strict read-only policy: write operations are forbidden by design.

use heapless::{String, Vec};

pub const MAX_GATT_SERVICES: usize = 12;
pub const MAX_GATT_CHARS: usize = 8;
pub const MAX_VALUE_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GattProperty {
    Read,
    Write,
    Notify,
    Indicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GattError {
    Timeout,
    WriteForbidden,
    ConnectionFailed,
    ReadFailed,
}

#[derive(Clone, Debug)]
pub struct GattCharacteristicInfo {
    pub uuid_text: String<40>,
    pub properties: Vec<GattProperty, 4>,
    pub readable: bool,
    pub value: Vec<u8, MAX_VALUE_BYTES>,
}

#[derive(Clone, Debug)]
pub struct GattServiceInfo {
    pub uuid_text: String<40>,
    pub characteristics: Vec<GattCharacteristicInfo, MAX_GATT_CHARS>,
}

#[derive(Clone, Debug, Default)]
pub struct GattExplorerResult {
    pub services: Vec<GattServiceInfo, MAX_GATT_SERVICES>,
}

/// Generic Read-Only GATT connection trait decoupling TrouBLE client specifics from UI.
pub trait ReadOnlyGattConnection {
    type Error;

    async fn discover_services(&mut self) -> Result<(), Self::Error>;
    async fn service_count(&self) -> usize;
    async fn service_uuid(&self, index: usize) -> Result<String<40>, Self::Error>;
    async fn characteristic_count(&self, service_index: usize) -> usize;
    async fn characteristic_uuid(
        &self,
        service_index: usize,
        characteristic_index: usize,
    ) -> Result<String<40>, Self::Error>;
    async fn characteristic_is_readable(
        &self,
        service_index: usize,
        characteristic_index: usize,
    ) -> bool;
    async fn read_characteristic(
        &mut self,
        service_index: usize,
        characteristic_index: usize,
        buffer: &mut [u8],
    ) -> Result<usize, Self::Error>;
    async fn disconnect(&mut self) -> Result<(), Self::Error>;
}

/// Traverses GATT hierarchy and performs strictly read-only reads of readable characteristics.
pub async fn explore_read_only<C>(connection: &mut C) -> Result<GattExplorerResult, C::Error>
where
    C: ReadOnlyGattConnection,
{
    connection.discover_services().await?;
    let service_count = connection.service_count().await;
    let mut result = GattExplorerResult::default();

    for service_index in 0..service_count {
        if result.services.is_full() {
            break;
        }

        let uuid_text = connection.service_uuid(service_index).await?;
        let mut service = GattServiceInfo {
            uuid_text,
            characteristics: Vec::new(),
        };

        let characteristic_count = connection.characteristic_count(service_index).await;

        for characteristic_index in 0..characteristic_count {
            if service.characteristics.is_full() {
                break;
            }

            let uuid_text = connection
                .characteristic_uuid(service_index, characteristic_index)
                .await?;

            let readable = connection
                .characteristic_is_readable(service_index, characteristic_index)
                .await;

            let mut value = Vec::new();
            if readable {
                let mut buffer = [0u8; MAX_VALUE_BYTES];
                if let Ok(length) = connection
                    .read_characteristic(service_index, characteristic_index, &mut buffer)
                    .await
                {
                    let safe_length = length.min(MAX_VALUE_BYTES);
                    let _ = value.extend_from_slice(&buffer[..safe_length]);
                }
            }

            let _ = service.characteristics.push(GattCharacteristicInfo {
                uuid_text,
                properties: Vec::new(),
                readable,
                value,
            });
        }

        let _ = result.services.push(service);
    }

    Ok(result)
}

pub struct GattPolicy {
    pub allow_reads: bool,
    pub allow_writes: bool,
    pub allow_notifications: bool,
}

impl Default for GattPolicy {
    fn default() -> Self {
        Self {
            allow_reads: true,
            allow_writes: false,
            allow_notifications: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockConnection {
        services: [&'static str; 2],
        chars: [&'static str; 2],
    }

    impl ReadOnlyGattConnection for MockConnection {
        type Error = ();

        async fn discover_services(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn service_count(&self) -> usize {
            self.services.len()
        }
        async fn service_uuid(&self, index: usize) -> Result<String<40>, Self::Error> {
            let mut s = String::new();
            let _ = s.push_str(self.services[index]);
            Ok(s)
        }
        async fn characteristic_count(&self, _service_index: usize) -> usize {
            self.chars.len()
        }
        async fn characteristic_uuid(
            &self,
            _service_index: usize,
            characteristic_index: usize,
        ) -> Result<String<40>, Self::Error> {
            let mut s = String::new();
            let _ = s.push_str(self.chars[characteristic_index]);
            Ok(s)
        }
        async fn characteristic_is_readable(
            &self,
            _service_index: usize,
            _characteristic_index: usize,
        ) -> bool {
            true
        }
        async fn read_characteristic(
            &mut self,
            _service_index: usize,
            _characteristic_index: usize,
            buffer: &mut [u8],
        ) -> Result<usize, Self::Error> {
            buffer[0] = 85; // e.g. 85%
            Ok(1)
        }
        async fn disconnect(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn policy_default_forbids_writes() {
        let policy = GattPolicy::default();
        assert!(policy.allow_reads);
        assert!(!policy.allow_writes);
        assert!(!policy.allow_notifications);
    }

    #[test]
    fn explore_read_only_discovers_mock() {
        let mut mock = MockConnection {
            services: ["0x180F (Battery)", "0x180A (Device Info)"],
            chars: ["0x2A19 (Level)", "0x2A24 (Model)"],
        };

        // Test synchronous invocation of async explore via embassy/futures
        embassy_futures::block_on(async {
            let res = explore_read_only(&mut mock).await.expect("success");
            assert_eq!(res.services.len(), 2);
            assert_eq!(res.services[0].characteristics.len(), 2);
            assert_eq!(&res.services[0].characteristics[0].value[..], &[85]);
        });
    }
}
