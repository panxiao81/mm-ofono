//! `org.freedesktop.ModemManager1.Sms`: one received or composed message.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt;
use zbus::{
    Connection, MatchRule, MessageStream, OwnedObjectPath, OwnedVariant, interface, message::Type,
    object_server::SignalEmitter,
};

use crate::error::{CoreError, Result};
use crate::mm::{MMSmsPduType as pdu, MMSmsState as state};
use crate::ofono::{self, SERVICE};

static NEXT_SMS: AtomicU32 = AtomicU32::new(0);

pub fn next_path() -> OwnedObjectPath {
    OwnedObjectPath::try_from(format!("{}/SMS/{}", crate::MM_PATH, NEXT_SMS.fetch_add(1, Ordering::Relaxed))).unwrap()
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SmsData {
    pub state: u32,
    pub pdu_type: u32,
    pub number: String,
    pub text: String,
    pub data: Vec<u8>,
    pub smsc: String,
    pub class: i32,
    pub delivery_report_request: bool,
    pub timestamp: String,
}

impl SmsData {
    /// A message oFono received.
    pub fn received(text: String, info: &ofono::Props, immediate: bool) -> SmsData {
        SmsData {
            state: state::MM_SMS_STATE_RECEIVED,
            pdu_type: pdu::MM_SMS_PDU_TYPE_DELIVER,
            number: ofono::str(info, "Sender").unwrap_or_default().to_owned(),
            text,
            // The service centre time stamp, in the sender's zone; ISO 8601 like ModemManager's.
            timestamp: ofono::str(info, "SentTime").unwrap_or_default().to_owned(),
            class: if immediate { 0 } else { -1 },
            ..Default::default()
        }
    }
    /// A WAP push oFono's PushNotification agent got: ModemManager's binary
    /// SMS, `Data` without text (mmsd-tng tells MMS notifications apart by that).
    pub fn push(data: Vec<u8>, info: &ofono::Props) -> SmsData {
        SmsData { data, ..SmsData::received(String::new(), info, false) }
    }
}

/// The data sits behind a mutex rather than `&mut self` so a slow Send
/// doesn't hold the interface's write lock and block property reads.
#[derive(Debug, Default)]
pub struct Sms {
    pub ofono_path: String,
    pub data: Mutex<SmsData>,
}

impl Sms {
    pub fn new(ofono_path: &str, data: SmsData) -> Sms {
        Sms { ofono_path: ofono_path.to_owned(), data: Mutex::new(data) }
    }

    fn get(&self) -> SmsData {
        self.data.lock().unwrap().clone()
    }

    async fn set_state(&self, em: &SignalEmitter<'_>, s: u32) -> zbus::Result<()> {
        self.data.lock().unwrap().state = s;
        self.state_changed(em).await
    }

    /// Hands the message to oFono and waits until oFono reports it sent or
    /// failed, like ModemManager's Send.
    async fn send_via_ofono(&self, conn: &Connection, number: &str, text: &str) -> Result<()> {
        // Subscribe before sending: oFono may finish (and drop the message
        // object) before SendMessage even returns.
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender(SERVICE)
            .path_namespace(self.ofono_path.clone())
            .interface("org.ofono.Message")
            .member("PropertyChanged")
            .build()?;
        let mut changes = MessageStream::for_match_rule(rule, conn, None).await?;

        let message: OwnedObjectPath = conn
            .call_method(Some(SERVICE), self.ofono_path.as_str(), Some("org.ofono.MessageManager"), "SendMessage", &(number, text))
            .await?
            .body()
            .deserialize()?;

        let outcome = tokio::time::timeout(Duration::from_secs(180), async {
            while let Some(Ok(msg)) = changes.next().await {
                if msg.header().path().map(|p| p.as_str()) != Some(message.as_str()) {
                    continue;
                }
                let Ok((key, value)) = msg.body().deserialize::<(String, OwnedVariant)>() else { continue };
                if key == "State" {
                    match String::try_from(value).as_deref() {
                        Ok("sent") => return Ok(()),
                        Ok("failed") => return Err(CoreError::Failed("oFono failed to send the message".into())),
                        _ => {}
                    }
                }
            }
            Err(CoreError::Failed("lost track of the message".into()))
        })
        .await;
        outcome.unwrap_or_else(|_| Err(CoreError::Failed("timed out sending the message".into())))
    }
}

#[interface(name = "org.freedesktop.ModemManager1.Sms")]
impl Sms {
    async fn send(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::MESSAGING).await?;
        let d = self.get();
        if !matches!(d.state, state::MM_SMS_STATE_UNKNOWN | state::MM_SMS_STATE_STORED) {
            return Err(CoreError::Failed("only new or stored messages can be sent".into()));
        }
        if d.text.is_empty() && !d.data.is_empty() {
            return Err(CoreError::Unsupported("sending data messages is not implemented".into()));
        }
        self.set_state(&em, state::MM_SMS_STATE_SENDING).await?;
        match self.send_via_ofono(conn, &d.number, &d.text).await {
            Ok(()) => Ok(self.set_state(&em, state::MM_SMS_STATE_SENT).await?),
            Err(e) => {
                self.set_state(&em, d.state).await?;
                Err(e)
            }
        }
    }

    async fn store(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _storage: u32) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::MESSAGING).await?;
        Err(CoreError::Unsupported("oFono keeps no message storage".into()))
    }

    #[zbus(property)]
    async fn state(&self) -> u32 {
        self.get().state
    }

    #[zbus(property)]
    async fn pdu_type(&self) -> u32 {
        self.get().pdu_type
    }

    #[zbus(property)]
    async fn number(&self) -> String {
        self.get().number
    }

    #[zbus(property)]
    async fn text(&self) -> String {
        self.get().text
    }

    #[zbus(property)]
    async fn data(&self) -> Vec<u8> {
        self.get().data
    }

    #[zbus(property, name = "SMSC")]
    async fn smsc(&self) -> String {
        self.get().smsc
    }

    #[zbus(property)]
    async fn validity(&self) -> (u32, OwnedVariant) {
        let unknown = crate::mm::MMSmsValidityType::MM_SMS_VALIDITY_TYPE_UNKNOWN;
        (unknown, OwnedVariant::from(0u32))
    }

    #[zbus(property)]
    async fn class(&self) -> i32 {
        self.get().class
    }

    #[zbus(property)]
    async fn teleservice_id(&self) -> u32 {
        crate::mm::MMSmsCdmaTeleserviceId::MM_SMS_CDMA_TELESERVICE_ID_UNKNOWN
    }

    #[zbus(property)]
    async fn service_category(&self) -> u32 {
        crate::mm::MMSmsCdmaServiceCategory::MM_SMS_CDMA_SERVICE_CATEGORY_UNKNOWN
    }

    #[zbus(property)]
    async fn delivery_report_request(&self) -> bool {
        self.get().delivery_report_request
    }

    #[zbus(property)]
    async fn message_reference(&self) -> u32 {
        0
    }

    #[zbus(property)]
    async fn timestamp(&self) -> String {
        self.get().timestamp
    }

    #[zbus(property)]
    async fn discharge_timestamp(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    async fn delivery_state(&self) -> u32 {
        crate::mm::MMSmsDeliveryState::MM_SMS_DELIVERY_STATE_UNKNOWN
    }

    #[zbus(property)]
    async fn storage(&self) -> u32 {
        crate::mm::MMSmsStorage::MM_SMS_STORAGE_UNKNOWN
    }
}
