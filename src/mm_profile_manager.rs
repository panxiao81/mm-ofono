//! `org.freedesktop.ModemManager1.Modem.Modem3gpp.ProfileManager`: oFono's
//! connection contexts as profiles (see [`crate::bearer::profile`]).

use std::collections::HashMap;

use zbus::{Connection, OwnedObjectPath, OwnedVariant, interface, object_server::SignalEmitter};

use crate::bearer::{self, CONTEXT, Contexts};
use crate::derive::CONNMAN;
use crate::error::{CoreError, Result};
use crate::ofono::{self, Props, SERVICE};

type Dict = HashMap<String, OwnedVariant>;

#[derive(Debug, Default)]
pub struct ProfileManager {
    pub ofono_path: String,
    pub contexts: Contexts,
}

impl ProfileManager {
    fn profiles(&self) -> Vec<Dict> {
        let contexts = self.contexts.lock().unwrap();
        let attach = bearer::attach_context(&contexts).map(|(p, _)| p.to_owned());
        let mut list: Vec<Dict> = contexts.iter().filter_map(|(p, c)| bearer::profile(p, c, attach.as_deref() == Some(p.as_str()))).collect();
        list.sort_by_key(|d| i32::try_from(d["profile-id"].try_clone().unwrap()).unwrap_or_default());
        list
    }

    /// The context path of the profile `requested` names by "profile-id".
    fn context_of(&self, requested: &Dict) -> Result<Option<String>> {
        let Some(id) = requested.get("profile-id") else { return Ok(None) };
        let id = i32::try_from(id.try_clone()?).map_err(|_| CoreError::InvalidArgs("profile-id must be an int32".into()))?;
        let contexts = self.contexts.lock().unwrap();
        contexts
            .keys()
            .find(|p| bearer::profile_id(p) == Some(id))
            .cloned()
            .map(Some)
            .ok_or_else(|| CoreError::NotFound(format!("no profile {id}")))
    }
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Modem3gpp.ProfileManager")]
impl ProfileManager {
    async fn list(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: zbus::message::Header<'_>) -> Result<Vec<Dict>> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        Ok(self.profiles())
    }

    /// Updates the profile with the given "profile-id", or adds a context.
    /// oFono refuses changes to an active context (InUse), as MM may.
    async fn set(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: zbus::message::Header<'_>, requested_properties: Dict) -> Result<Dict> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let changes = bearer::profile_settings(&requested_properties).map_err(CoreError::InvalidArgs)?;
        let path = match self.context_of(&requested_properties)? {
            Some(p) => p,
            None => {
                let ty = bearer::Request::from_dict(&requested_properties).apn_type;
                conn.call_method(Some(SERVICE), self.ofono_path.as_str(), Some(CONNMAN), "AddContext", &(bearer::profile_context_type(ty),))
                    .await?
                    .body()
                    .deserialize::<OwnedObjectPath>()?
                    .to_string()
            }
        };
        for (key, value) in changes {
            ofono::set_property(conn, &path, CONTEXT, key, value.as_str().into()).await?;
        }
        // The stored settings, as oFono now has them.
        let ctx: Props = ofono::get_properties(conn, &path, CONTEXT).await?;
        self.contexts.lock().unwrap().insert(path.clone(), ctx);
        let contexts = self.contexts.lock().unwrap();
        let attach = bearer::attach_context(&contexts).is_some_and(|(p, _)| p == path);
        bearer::profile(&path, &contexts[&path], attach).ok_or_else(|| CoreError::Failed(format!("{path} isn't a context path")))
    }

    async fn delete(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: zbus::message::Header<'_>, properties: Dict) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let path = self.context_of(&properties)?.ok_or_else(|| CoreError::InvalidArgs("profile-id is required".into()))?;
        let path = OwnedObjectPath::try_from(path)?;
        conn.call_method(Some(SERVICE), self.ofono_path.as_str(), Some(CONNMAN), "RemoveContext", &(path,)).await?;
        Ok(())
    }

    /// oFono added or removed a context.
    // ponytail: also emitted for contexts our own Set/Delete added or
    // removed, which MM avoids; clients reload with List() either way.
    #[zbus(signal)]
    pub async fn updated(em: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property)]
    async fn index_field(&self) -> &str {
        "profile-id"
    }
}
