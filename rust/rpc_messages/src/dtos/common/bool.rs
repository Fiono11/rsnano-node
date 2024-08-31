use serde::{
    de::{self, MapAccess, Visitor},
    ser::SerializeMap,
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub struct BoolDto {
    pub key: String,
    pub value: bool,
}

impl BoolDto {
    pub fn new(key: String, value: bool) -> Self {
        Self { key, value }
    }
}

impl Serialize for BoolDto {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(&self.key, &self.value)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for BoolDto {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BoolMessageDtoVisitor;

        impl<'de> Visitor<'de> for BoolMessageDtoVisitor {
            type Value = BoolDto;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a map with a single key-value pair where value is a bool")
            }

            fn visit_map<A>(self, mut map: A) -> Result<BoolDto, A::Error>
            where
                A: MapAccess<'de>,
            {
                let (key, value): (String, bool) = match map.next_entry()? {
                    Some(pair) => pair,
                    None => {
                        return Err(de::Error::invalid_length(0, &self));
                    }
                };

                if map.next_entry::<String, bool>()?.is_some() {
                    return Err(de::Error::custom(
                        "Found more than one key-value pair in the map",
                    ));
                }

                Ok(BoolDto { key, value })
            }
        }

        deserializer.deserialize_map(BoolMessageDtoVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{self, from_str};

    #[test]
    fn serialize_true() {
        let dto = BoolDto {
            key: String::from("test_key"),
            value: true,
        };

        let serialized = serde_json::to_string(&dto).unwrap();

        let expected = r#"{"test_key":true}"#;

        assert_eq!(serialized, expected);
    }

    #[test]
    fn serialize_false() {
        let dto = BoolDto {
            key: String::from("another_key"),
            value: false,
        };

        let serialized = serde_json::to_string(&dto).unwrap();

        let expected = r#"{"another_key":false}"#;

        assert_eq!(serialized, expected);
    }

    #[test]
    fn deserialize_true() {
        let json_str = r#"{"key1": true}"#;
        let deserialized: BoolDto = from_str(json_str).unwrap();
        assert_eq!(
            deserialized,
            BoolDto {
                key: "key1".to_string(),
                value: true,
            }
        );
    }

    #[test]
    fn deserialize_false() {
        let json_str = r#"{"key_false": false}"#;
        let deserialized: BoolDto = from_str(json_str).unwrap();
        assert_eq!(
            deserialized,
            BoolDto {
                key: "key_false".to_string(),
                value: false,
            }
        );
    }
}