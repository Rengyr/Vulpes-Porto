use std::{
   fmt::{Display, Formatter},
   fs::File,
   io::Write,
};

use serde::{de::Error, Deserialize, Deserializer, Serialize};

pub enum GetImageErrorLevel {
   Normal(anyhow::Error),
   Critical(anyhow::Error),
}

#[derive(Serialize, Deserialize, Debug, PartialOrd, PartialEq, Ord, Eq)]
pub enum MessageLevel {
   Emergency = 0,
   Alert = 1,
   Critical = 2,
   Error = 3,
   Warning = 4,
   Notice = 5,
   Info = 6,
   Debug = 7,
}

pub enum MessageOutput {
   Stdout,
   Stderr,
}

#[derive(Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum StatusVisibility {
   Public,
   Unlisted,
   Private,
   Direct,
   Default,
}

impl Display for StatusVisibility {
   fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
      match self {
         StatusVisibility::Public => write!(f, "public"),
         StatusVisibility::Unlisted => write!(f, "unlisted"),
         StatusVisibility::Private => write!(f, "private"),
         StatusVisibility::Direct => write!(f, "direct"),
         StatusVisibility::Default => write!(f, ""),
      }
   }
}

///Structure holding configuration of the bot
#[derive(Deserialize, Debug)]
pub struct Config {
   pub server: String,
   pub token: String,
   pub image_json: String,
   #[serde(alias = "not_used_images_log_location")]
   pub internal_database: String,
   #[serde(deserialize_with = "from_string_time")]
   pub times: Vec<(u8, u8)>,
   #[serde(default)]
   pub tags: String,
   pub local_path: Option<String>,
   pub use_syslog_style: Option<bool>,
   #[serde(default = "default_log_level")]
   pub log_level: MessageLevel,
   #[serde(default = "default_retry_time")]
   pub retry_time: u64,
   #[serde(default = "default_status_visibility")]
   pub status_visibility: StatusVisibility,
   #[serde(default, deserialize_with = "from_status_visibility_sequence")]
   pub status_visibility_sequence: Option<Vec<StatusVisibility>>,
}

fn default_log_level() -> MessageLevel {
   MessageLevel::Info
}

fn default_retry_time() -> u64 {
   10 * 60 // 10 minutes
}

fn default_status_visibility() -> StatusVisibility {
   StatusVisibility::Default
}

impl Config {
   /// Function to print message with correct level, output and systemd prefix if needed
   /// * `message` - Message to be printed
   /// * `level` - Level of the message
   /// * `output` - Output to be used
   pub fn output_message(&self, message: &str, level: MessageLevel, output: MessageOutput) {
      // Check if message level is enough to be outputted
      if level > self.log_level {
         return;
      }

      // First append systemd prefix if needed based on message level and then write to correct output
      let message = match self.use_syslog_style {
         Some(true) => {
            let prefix = format!("<{}>", level as u8);
            format!("{}{}", prefix, message)
         }
         _ => message.to_string(),
      };

      match output {
         MessageOutput::Stdout => println!("{}", message),
         MessageOutput::Stderr => eprintln!("{}", message),
      };
   }

   /// Function to panic with correct systemd level if needed
   /// * `message` - Message to be panicked with
   /// * `level` - Level of the message
   pub fn panic_message(&self, message: &str, level: MessageLevel) -> ! {
      // Append systemd prefix if needed based on message level and then panic
      let message = match self.use_syslog_style {
         Some(true) => {
            let prefix = format!("<{}>", level as u8);
            format!("{}{}", prefix, message)
         }
         _ => message.to_string(),
      };
      panic!("{}", message);
   }
}

fn from_string_time<'de, D>(deserializer: D) -> Result<Vec<(u8, u8)>, D::Error>
where
   D: Deserializer<'de>,
{
   let s: Vec<&str> = Deserialize::deserialize(deserializer)?;

   // Deserializing time to tuple with hours and minutes
   s.into_iter()
      .map(|time| -> Result<(u8, u8), <D as Deserializer>::Error> {
         let mut time_split = time.split(':');
         let hours = time_split
            .next()
            .ok_or_else(|| D::Error::custom("missing hours"))
            .and_then(|h| h.parse::<u8>().map_err(|_| D::Error::custom("can't parse hours")));
         let minutes = time_split
            .next()
            .ok_or_else(|| D::Error::custom("missing minutes"))
            .and_then(|h| h.parse::<u8>().map_err(|_| D::Error::custom("can't parse minutes")));
         match (hours, minutes) {
            (Ok(hours), Ok(minutes)) => {
               if hours > 23 {
                  Err(D::Error::custom("hours must be less than 23"))
               } else if minutes > 60 {
                  Err(D::Error::custom("minutes must be less than 60"))
               } else {
                  Ok((hours, minutes))
               }
            }
            (Err(hours), Ok(_)) => Err(hours),
            (Ok(_), Err(minutes)) => Err(minutes),
            _ => Err(D::Error::custom("invalid time")),
         }
      })
      .collect::<Result<Vec<(u8, u8)>, D::Error>>()
}

#[derive(Deserialize)]
#[serde(untagged)]
enum VisibilitySequenceItem {
   Simple(StatusVisibility),
   Pair(StatusVisibility, usize),
}

fn from_status_visibility_sequence<'de, D>(deserializer: D) -> Result<Option<Vec<StatusVisibility>>, D::Error>
where
   D: Deserializer<'de>,
{
   let deserialized: Result<Vec<VisibilitySequenceItem>, _> = Deserialize::deserialize(deserializer);

   match deserialized {
      Ok(seq) => {
         let mut simple_seq = Vec::new();
         for item in seq {
            match item {
               VisibilitySequenceItem::Simple(visibility) => simple_seq.push(visibility),
               VisibilitySequenceItem::Pair(visibility, amount) => {
                  for _ in 0..amount {
                     simple_seq.push(visibility.clone());
                  }
               }
            }
         }
         Ok(Some(simple_seq))
      }
      Err(_) => Err(D::Error::custom(
         "Wrong format of status_visibility_sequence. \
              Allowed simple array or array with tuples of visibility and amount, e.g: \
              [\"default\", \"default\", \"public\"] or [[\"default\", 2], [\"public\",1]]",
      )),
   }
}

///Structure containing info about the image
#[derive(Serialize, Deserialize, Debug)]
pub struct Image {
   /// Optional message
   pub msg: Option<String>,
   /// Optional alt text for image
   pub alt: Option<String>,
   /// Optional content warning
   pub content_warning: Option<String>,
   /// Link to hosted image
   pub location: String,
}

impl Image {
   pub fn get_hash(&self) -> String {
      format!("{:x}", md5::compute(&self.location))
   }
}

///Structure containing info about current used and unused images
#[derive(Serialize, Deserialize, Debug)]
pub struct ImageDB {
   // List of used images
   pub used: Vec<String>,
   // List of unused images
   pub unused: Vec<String>,
   // List of random deck for picking images after all were used
   pub random_deck: Vec<String>,
   #[serde(default)]
   pub visiblity_sequence: usize,
}

impl ImageDB {
   /// Check if the hash is in the used or unused list
   /// * `hash` - Hash to check
   pub fn contains(&self, hash: &String) -> bool {
      self.used.contains(hash) || self.unused.contains(hash)
   }
}

///Save used and unused images to file.
pub fn save_images_ids(internal_db: &mut ImageDB, app_config: &Config) {
   match File::create(app_config.internal_database.clone()) {
      Ok(mut file) => {
         file.write_all(serde_json::to_string(&internal_db).unwrap().as_bytes()).unwrap();
      }
      Err(e) => {
         app_config.output_message(
            &format!("Unable to create not_used_images_log_location.\nError: {:#}", e),
            MessageLevel::Error,
            MessageOutput::Stderr,
         );
      }
   };
}
