use aes_gcm::AeadInPlace;
use aes_gcm::NewAead;
use aes_gcm::{Aes256Gcm, Key as AesKey, Nonce};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

type KeyLen = generic_array::typenum::U32;
type NonceLen = generic_array::typenum::U12;

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub sender: String,
    pub msg: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EncryptedMessage {
    nonce: Nonce<NonceLen>,
    data: Vec<u8>,
}

pub struct Key {
    key: AesKey<KeyLen>,
}

impl Message {
    pub fn new(sender: String, msg: String) -> Message {
        Message { sender, msg }
    }

    pub fn encrypt(&self, key: &Key) -> EncryptedMessage {
        let cipher = Aes256Gcm::new(&key.key);
        let nonce = Nonce::<NonceLen>::from_slice(&rand::rngs::OsRng.gen::<[u8; 12]>()).clone();
        let mut serialized = bincode::serialize(&self).expect("Serialization can't fail");
        cipher
            .encrypt_in_place(&nonce, b"", &mut serialized)
            .expect("encryption failure");

        EncryptedMessage {
            nonce,
            data: serialized,
        }
    }

    pub fn decrypt(msg: EncryptedMessage, key: &Key) -> Result<Message, ()> {
        let mut serialized = msg.data;
        let cipher = Aes256Gcm::new(&key.key);
        cipher
            .decrypt_in_place(&msg.nonce, b"", &mut serialized)
            .map_err(|_| ())?;

        bincode::deserialize(&serialized).map_err(|_| ())
    }
}

impl FromStr for Key {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s)?;
        if bytes.len() != 32 {
            return Err(anyhow::Error::msg("wrong key length"));
        }
        Ok(Key {
            key: *aes_gcm::Key::<KeyLen>::from_slice(&bytes),
        })
    }
}
