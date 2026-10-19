//! 旧 Goal/Step 快照的窄读取迁移。写出只含当前字段，不保留旧预算控制状态。
macro_rules! persisted_without_task_budget {
    ($name:ident, $remote:literal, [$($retired:literal),*], { $($fields:tt)* }) => {
        #[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $($fields)* }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                #[derive(serde::Deserialize)]
                #[serde(remote = $remote, rename_all = "camelCase", deny_unknown_fields)]
                struct Current { $($fields)* }

                let mut value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
                if let Some(object) = value.as_object_mut() {
                    $(object.remove($retired);)*
                }
                Current::deserialize(value).map_err(serde::de::Error::custom)
            }
        }
    };
}
