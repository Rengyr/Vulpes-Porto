#[macro_use]
extern crate version;

use core::time;
use std::{
   collections::HashMap,
   env,
   fs::{self, File},
   io::{BufReader, Read, Write},
   path::Path,
   thread,
   time::Instant,
};

use rand::Rng;
use serde::{de::Error, Deserialize, Deserializer, Serialize};

use chrono::{DateTime, Local, NaiveTime, TimeZone, Utc};

use reqwest::blocking::multipart::{self, Part};
use serde_json::Value;

use anyhow::{anyhow, Result};

static GITHUB_LINK: &str = "https://github.com/Rengyr/Vulpes-Porto";

enum GetImageErrorLevel {
   Normal(anyhow::Error),
   Critical(anyhow::Error),
}

#[derive(Serialize, Deserialize, Debug, PartialOrd, PartialEq, Ord, Eq)]
enum MessageLevel {
   Emergency = 0,
   Alert = 1,
   Critical = 2,
   Error = 3,
   Warning = 4,
   Notice = 5,
   Info = 6,
   Debug = 7,
}

enum MessageOutput {
   Stdout,
   Stderr,
}

///Structure holding configuration of the bot
#[derive(Serialize, Deserialize, Debug)]
struct Config {
   server: String,
   token: String,
   image_json: String,
   not_used_images_log_location: String,
   #[serde(deserialize_with = "from_string_time")]
   times: Vec<(u8, u8)>,
   tags: Option<String>,
   local_path: Option<String>,
   use_syslog_style: Option<bool>,
   log_level: Option<MessageLevel>,
   retry_time: Option<u64>, // Time in seconds to wait before retrying to post image
}

impl Config {
   /// Function to print message with correct level, output and systemd prefix if needed
   fn output_message(&self, message: &str, level: MessageLevel, output: MessageOutput) {
      // Check if message level is enough to be outputted
      if let Some(min_level) = self.log_level.as_ref() {
         if &level > min_level {
            return;
         }
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
   fn panic_message(&self, message: &str, level: MessageLevel) -> ! {
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

   //Deserializing time to tuple with hours and minutes
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

///Structure containing info about the image
#[derive(Serialize, Deserialize, Debug)]
struct Image {
   msg: Option<String>,             //Optional message
   alt: Option<String>,             //Optional alt text for image
   content_warning: Option<String>, //Optional content warning
   location: String,                //Link to hosted image
}

///Structure containing info about current used and unused images
#[derive(Serialize, Deserialize, Debug)]
struct ImageDB {
   used: Vec<String>,
   unused: Vec<String>,
   random_deck: Vec<String>,
}

impl ImageDB {
   ///Check if the hash is in the used or unused list
   pub fn contains(&self, hash: &String) -> bool {
      self.used.contains(hash) || self.unused.contains(hash)
   }
}

///From link to json load new image and parse the results to ImageDB structure. Returns Hashmap with images with keys of md5 hashes or returns Error.
fn load_images(
   app_config: &Config,
   images_db: &mut ImageDB,
   images_old: Option<&HashMap<String, Image>>,
) -> Result<HashMap<String, Image>> {
   //Check if path is system one or website one
   let images_json = match Path::new(&app_config.image_json).exists() {
      true => {
         // Allow to use "file:" prefix for local json file
         let image_json_path = app_config.image_json.strip_prefix("file:").unwrap_or(&app_config.image_json);

         //Load the json file from disk
         match fs::read_to_string(image_json_path) {
            Ok(images_json) => images_json,
            Err(err) => return Err(anyhow!(err).context("Unable to read json file with images")),
         }
      }
      false => {
         let client_media = match reqwest::blocking::Client::builder()
            .user_agent("VulpesPorto/".to_string() + version!() + " (" + GITHUB_LINK + ")")
            .build()
         {
            Ok(client_media) => client_media,
            Err(err) => {
               return Err(anyhow!(err).context("Unable to make reqwest client for remote json file with images"));
            }
         };

         //Get json file from remote location
         let result = match client_media.get(&app_config.image_json).send() {
            Ok(result) => result,
            Err(err) => {
               return Err(
                  anyhow!(err).context("Unable to find json file with images (either local path or web address is wrong)"),
               )
            }
         };

         //Parse remote file as text
         match result.text() {
            Ok(images_json) => images_json,
            Err(err) => return Err(anyhow!(err).context("Unable parse web result of json file with images as text")),
         }
      }
   };

   //Parse json to images
   let images: Vec<Image> = match serde_json::from_str(&images_json) {
      Ok(images) => images,
      Err(err) => return Err(anyhow!(err).context("Unable to parse text as images json")),
   };

   //Calculate md5 hashes as keys for duplicity check
   let images_hashes: Vec<(String, String)> =
      images.iter().map(|image| (format!("{:x}", md5::compute(&image.location)), image.location.clone())).collect();

   // Keep list of reported duplicates to avoid duplicate warnings
   let mut reported_duplicates = Vec::new();
   for (hash, location) in images_hashes.iter() {
      let mut duplicate_counter = 0;

      // Iterate over all images with the same hash
      for (index_duplicate, _) in images_hashes.iter().enumerate().filter(|(_, (list_hash, _))| list_hash == hash) {
         duplicate_counter += 1;
         if duplicate_counter > 1 {
            // If this hash has already been reported, skip it
            if reported_duplicates.contains(&index_duplicate) {
               continue;
            }

            reported_duplicates.push(index_duplicate);

            // Get index of line for duplicate image
            let duplicite_line = images_json
               .split('\n')
               .enumerate()
               .filter(|(_, line_string)| line_string.contains(location))
               .nth(duplicate_counter - 1)
               .unwrap()
               .0
               + 1;

            // Get index for original image
            let image_line =
               images_json.split('\n').enumerate().find(|(_, line_string)| line_string.contains(location)).unwrap().0 + 1;

            app_config.output_message(
               &format!("Image at line {} is duplicate, first seen at line {} [{}]", duplicite_line, image_line, location),
               MessageLevel::Warning,
               MessageOutput::Stdout,
            );
         }
      }
   }

   //Calculate md5 hashes as keys for images
   let images: HashMap<String, Image> =
      images.into_iter().map(|image| (format!("{:x}", md5::compute(&image.location)), image)).collect();

   //Add new images to unused list
   let mut new = 0;
   for hash in images.keys() {
      if !images_db.contains(hash) {
         images_db.unused.push(hash.to_owned());
         new += 1;
      }
   }
   if new > 0 {
      app_config.output_message(&format!("Added {} new images", new), MessageLevel::Notice, MessageOutput::Stdout);
   }

   //Remove images that were removed from json from the unused list
   let mut removed = 0;
   images_db.unused.retain(|hash| {
      if !images.contains_key(hash) {
         removed += 1;
         false
      } else {
         true
      }
   });
   images_db.used.retain(|hash| {
      if !images.contains_key(hash) {
         removed += 1;
         false
      } else {
         true
      }
   });
   if removed > 0 {
      app_config.output_message(
         &format!("Removed {} images not found in json", removed),
         MessageLevel::Notice,
         MessageOutput::Stdout,
      );
   }

   //Remove images that were removed from json from random deck
   let mut removed_d = 0;
   images_db.random_deck.retain(|hash| {
      if !images.contains_key(hash) {
         removed_d += 1;
         false
      } else {
         true
      }
   });
   if removed_d > 0 {
      app_config.output_message(
         &format!("Removed from random deck {} images not found in json", removed_d),
         MessageLevel::Notice,
         MessageOutput::Stdout,
      );
   }

   //Check if alt text or text of images changed and write notice to stdout
   if let Some(images_old) = images_old {
      let mut message_changed = 0;
      let mut alt_changed = 0;
      let mut content_warning_changed = 0;
      for image in &images {
         if let Some(image_old) = images_old.get(image.0) {
            if image_old.msg != image.1.msg {
               message_changed += 1;
            }
            if image_old.alt != image.1.alt {
               alt_changed += 1;
            }
            if image_old.content_warning != image.1.content_warning {
               content_warning_changed += 1;
            }
         }
      }
      if message_changed > 0 {
         app_config.output_message(
            &format!("Text changed for {} images", message_changed),
            MessageLevel::Notice,
            MessageOutput::Stdout,
         );
      }
      if alt_changed > 0 {
         app_config.output_message(
            &format!("Alt text changed for {} images", alt_changed),
            MessageLevel::Notice,
            MessageOutput::Stdout,
         );
      }
      if content_warning_changed > 0 {
         app_config.output_message(
            &format!("Content warning changed for {} images", content_warning_changed),
            MessageLevel::Notice,
            MessageOutput::Stdout,
         );
      }
   }

   Ok(images)
}

/// Return next closest time that is in the future given times in config or current time + 1 day if no times are configured.
///
/// Times in config has to be sorted.
#[allow(deprecated)]
fn get_next_time<Tz: TimeZone>(date_time: DateTime<Tz>, config: &Config) -> DateTime<Tz> {
   if config.times.is_empty() {
      return date_time + chrono::Duration::days(1);
   }

   let mut current_date = date_time.date();
   let date_time_now: DateTime<Tz> = Utc::now().with_timezone(&date_time.timezone());

   //Loop until time is found
   loop {
      //Try all times in the config
      for (hours, minutes) in config.times.iter() {
         let post_date_time = match current_date.and_hms_opt(hours.to_owned() as u32, minutes.to_owned() as u32, 0) {
            Some(new_date_time) => new_date_time,
            None => {
               if *hours <= 23 && *minutes <= 59 {
                  // Check if we need to show the information about skipped time or if it's no longer relevant
                  let Some(time) = NaiveTime::from_hms_opt(*hours as u32, *minutes as u32, 0) else {
                     // Could be panic here, but no reason to crash service just when trying to see if to print message (and this shouldn't happen anyway)
                     config.output_message(
                        &format!("Can't make time {}:{} while checking Daylight Saving Time", hours, minutes),
                        MessageLevel::Warning,
                        MessageOutput::Stdout,
                     );
                     continue;
                  };
                  if current_date > date_time.date() || (current_date == date_time.date() && time > date_time.time()) {
                     config.output_message(
                        &format!("Skipped time {}:{} because it doesn't exist due to Daylight Saving Time", hours, minutes),
                        MessageLevel::Info,
                        MessageOutput::Stdout,
                     );
                  }
                  continue; //Hours and minutes are correct, but probably daylight saving time make the specific time not exist
               }
               config.panic_message(
                  &format!("Invalid hours or minutes in the configuration: hours: {}, minutes: {}", hours, minutes),
                  MessageLevel::Critical,
               );
            }
         };

         if date_time_now < post_date_time {
            return post_date_time;
         }
      }

      //Add one day if no time in config is in the future for current day
      current_date += chrono::Duration::days(1);
   }
}

///Fetch image and get it as bytes ready to be send to API
fn get_image(local_path: Option<&String>, image_path: &str) -> Result<Vec<u8>, GetImageErrorLevel> {
   // Check if it is local image
   if let Some(image_path) = image_path.strip_prefix("file:") {
      let Some(local_path) = local_path else {
         return Err(GetImageErrorLevel::Critical(anyhow!("Missing local path in configuration file")));
      };
      let local_path_struct = Path::new(local_path);
      let path = local_path_struct.join(image_path);

      // Check if path exist
      if !path.exists() {
         return match path.into_os_string().into_string() {
            Ok(path) => Err(GetImageErrorLevel::Critical(anyhow!("Local path is wrong: {}", path))),
            Err(_) => Err(GetImageErrorLevel::Critical(anyhow!("Not correct OS path for: {}", image_path))),
         };
      }

      // Directory traversal attack mitigation
      let path = match path.canonicalize() {
         Ok(path) => path,
         Err(error) => {
            return Err(GetImageErrorLevel::Critical(anyhow!(
               "Can't make canonical absolute path for image {}: {:#}",
               image_path,
               error
            )))
         }
      };
      let local_canon_path = match local_path_struct.canonicalize() {
         Ok(path) => path,
         Err(error) => {
            return Err(GetImageErrorLevel::Critical(anyhow!(
               "Can't make canonical absolute path for local path {}: {:#}",
               local_path,
               error
            )))
         }
      };
      if !path.starts_with(local_canon_path) {
         return Err(GetImageErrorLevel::Critical(anyhow!(
            "Directory traversal is not permitted for local image {}",
            image_path
         )));
      }

      // Read file
      let mut bytes: Vec<u8> = Vec::new();
      let mut file = match File::open(path) {
         Ok(file) => file,
         Err(error) => return Err(GetImageErrorLevel::Critical(anyhow!("Can't open image {}: {:#}", image_path, error))),
      };

      match file.read_to_end(&mut bytes) {
         Ok(_) => {}
         Err(error) => {
            return Err(GetImageErrorLevel::Normal(anyhow!("Error during reading image {}: {:+}", image_path, error)));
         }
      };

      Ok(bytes)
   // Remote image
   } else {
      //Make client for request
      let client_media = match reqwest::blocking::Client::builder()
         .user_agent("VulpesPorto/".to_string() + version!() + " (" + GITHUB_LINK + ")")
         .build()
      {
         Ok(client_media) => client_media,
         Err(e) => {
            return Err(GetImageErrorLevel::Normal(anyhow!("Unable to initialize client to fetch remote images: {:#}", e)));
         }
      };

      //Download image to cache
      let response = match client_media.get(image_path).send() {
         Ok(response) => response,
         Err(e) => {
            return Err(GetImageErrorLevel::Normal(anyhow!("Unable to get remote image {}: {:#}", image_path, e)));
         }
      };

      if response.status() == 401 || response.status() == 403 || response.status() == 404 {
         return Err(GetImageErrorLevel::Critical(anyhow!(
            "Client error response when getting remote image {}: {}",
            image_path,
            response.status()
         )));
      }

      match response.bytes() {
         Ok(bytes) => Ok(bytes.into_iter().collect()),
         Err(error) => {
            Err(GetImageErrorLevel::Normal(anyhow!("Response from remote image {} request is wrong: {:#}", image_path, error)))
         }
      }
   }
}

///Send request for new media post to Mastodon server and return error if there is any.
fn post_image<'a>(app_config: &Config, images: &'a HashMap<String, Image>, images_db: &mut ImageDB) -> Result<&'a Image, ()> {
   let rng = &mut rand::thread_rng();

   //Get random hash from unused if there is any else from random deck
   let image_hash = match images_db.unused.is_empty() {
      true => {
         if images_db.random_deck.is_empty() {
            images_db.random_deck.append(&mut images_db.used.to_vec());
            app_config.output_message("Random deck was shuffled", MessageLevel::Debug, MessageOutput::Stdout);
         }
         images_db.random_deck.get(rng.gen_range(0..images_db.random_deck.len())).unwrap().to_owned()
      }
      false => images_db.unused.get(rng.gen_range(0..images_db.unused.len())).unwrap().to_owned(),
   };

   //Get image from hash
   let image = match images.get(&image_hash) {
      Some(image) => image,
      None => {
         app_config.output_message(
            &format!("Can't find image with hash {}", image_hash),
            MessageLevel::Error,
            MessageOutput::Stderr,
         );
         return Err(());
      }
   };

   let bytes = get_image(app_config.local_path.as_ref(), &image.location);

   let bytes = match bytes {
      Ok(bytes) => bytes,
      Err(error) => {
         let error_message = match &error {
            GetImageErrorLevel::Normal(message) => message,
            GetImageErrorLevel::Critical(message) => message,
         };
         app_config.output_message(&format!("{:#}", error_message), MessageLevel::Error, MessageOutput::Stderr);

         //Remove image for critical errors
         if matches!(error, GetImageErrorLevel::Critical { .. }) {
            match images_db.unused.is_empty() {
               true => {
                  let pos = images_db.random_deck.iter().position(|hash| hash == &image_hash).unwrap();
                  images_db.random_deck.remove(pos);
               }
               false => {
                  let pos = images_db.unused.iter().position(|hash| hash == &image_hash).unwrap();
                  images_db.unused.remove(pos);
                  images_db.used.push(image_hash.to_owned());
               }
            };
         }

         return Err(());
      }
   };

   let part = Part::bytes(bytes).file_name("image");

   let client = match reqwest::blocking::Client::builder()
      .user_agent("VulpesPorto/".to_string() + version!() + " (" + GITHUB_LINK + ")")
      .build()
   {
      Ok(client) => client,
      Err(e) => {
         app_config.output_message(
            &format!("Unable to initialize client to post image to /api/v2/media: {:#}", e),
            MessageLevel::Error,
            MessageOutput::Stderr,
         );
         return Err(());
      }
   };

   //Construct request to upload image to mastodon and get media id
   let mut media_request = multipart::Form::new()
      // Image
      .part("file", part);

   if let Some(alt) = image.alt.clone() {
      media_request = media_request.text("description", alt);
   }

   let response = client
      .post(app_config.server.to_owned() + "/api/v2/media")
      .header("Authorization", "Bearer ".to_string() + app_config.token.to_string().as_str())
      .multipart(media_request)
      .send();

   let response = match response {
      Ok(response) => response,
      Err(e) => {
         app_config.output_message(
            &format!("Unable to post image to /api/v2/media for image {}.\nError: {:#}", image.location, e),
            MessageLevel::Error,
            MessageOutput::Stderr,
         );
         return Err(());
      }
   };

   if !response.status().is_success() {
      app_config.output_message(
         &format!("Wrong status from media api: {} for image {}", response.status(), image.location),
         MessageLevel::Error,
         MessageOutput::Stderr,
      );
      app_config.output_message(&format!("Response: {}", response.text().unwrap()), MessageLevel::Error, MessageOutput::Stderr);
      return Err(());
   }

   let media_json: Value = match serde_json::from_str(&response.text().unwrap()) {
      Ok(media_json) => media_json,
      Err(e) => {
         app_config.output_message(
            &format!("Unable to parse media json for image {}.\nError: {:#}", image.location, e),
            MessageLevel::Error,
            MessageOutput::Stderr,
         );
         return Err(());
      }
   };

   let media_id: String = match media_json["id"].as_str() {
      Some(media_id) => media_id.to_string(),
      None => {
         app_config.output_message(
            &format!("Unable to get media id: {:?} for image {}", media_json, image.location),
            MessageLevel::Error,
            MessageOutput::Stderr,
         );
         return Err(());
      }
   };

   //Construct request to post new post to mastodon with the image
   let mut status_request = multipart::Form::new()
      // Image id
      .text("media_ids[]", media_id);

   //Get the message on the image or default ""
   let mut message = image.msg.clone().unwrap_or_default();

   //If tags are specified then add tags after new line if message is not empty
   if let Some(tags) = app_config.tags.as_ref() {
      if !message.is_empty() && !tags.is_empty() {
         message += "\n";
      }
      message += tags;
   }

   //Add message to the posted image if there is something
   if !message.is_empty() {
      status_request = status_request.text("status", message);
   }

   //Add context warning to the posted image if there is something
   if let Some(content_warning) = &image.content_warning {
      status_request = status_request.text("spoiler_text", content_warning.to_owned());
   }

   let response = client
      .post(app_config.server.to_owned() + "/api/v1/statuses")
      .header("Authorization", "Bearer ".to_string() + app_config.token.to_string().as_str())
      .multipart(status_request)
      .send();

   let response = match response {
      Ok(response) => response,
      Err(e) => {
         app_config.output_message(
            &format!("Unable to post image to /api/v1/statuses for image {}.\nError: {:#}", image.location, e),
            MessageLevel::Error,
            MessageOutput::Stderr,
         );
         return Err(());
      }
   };

   if !response.status().is_success() {
      app_config.output_message(
         &format!("Wrong status from statuses api: {} for image {}", response.status(), image.location),
         MessageLevel::Error,
         MessageOutput::Stderr,
      );
      return Err(());
   }

   //Remove hash from the lists
   match images_db.unused.is_empty() {
      true => {
         let pos = images_db.random_deck.iter().position(|hash| hash == &image_hash).unwrap();
         images_db.random_deck.remove(pos);
      }
      false => {
         let pos = images_db.unused.iter().position(|hash| hash == &image_hash).unwrap();
         images_db.unused.remove(pos);
         images_db.used.push(image_hash.to_owned());
      }
   };

   Ok(image)
}

///Save used and unused images to file.
fn save_images_ids(image_db: &mut ImageDB, app_config: &Config) {
   match File::create(app_config.not_used_images_log_location.clone()) {
      Ok(mut file) => {
         file.write_all(serde_json::to_string(&image_db).unwrap().as_bytes()).unwrap();
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

fn main() {
   let args: Vec<String> = env::args().collect();

   if args.len() < 2 {
      println!("Usage: {} <config.json> [--now]", args[0]);
      return;
   }

   //Load bot configuration
   let app_config = fs::read_to_string(&args[1]).unwrap_or_else(|_| panic!("Couldn't find config.json file"));

   let mut app_config: Config =
      serde_json::from_str(&app_config).unwrap_or_else(|e| panic!("Unable to parse config.json: {}", e));
   app_config.times.sort_unstable();

   //Parse systemd first to use it for further messages
   for arg in args.iter().skip(2) {
      //Set systemd output style
      if arg == "--systemd" {
         app_config.use_syslog_style = Some(true);
         app_config.output_message(
            "Using --systemd is deprecated, use setting in configuration file instead",
            MessageLevel::Notice,
            MessageOutput::Stdout,
         );
      }
   }

   //Load used and unused list of images
   let mut not_used_images = match File::open(app_config.not_used_images_log_location.clone()) {
      Ok(file) => {
         let reader = BufReader::new(file);
         match serde_json::from_reader(reader) {
            Ok(res) => res,
            Err(e) => {
               app_config.panic_message(&format!("Unable to parse not_used_images_log.\nError: {:#}", e), MessageLevel::Critical);
            }
         }
      }
      Err(_) => ImageDB { used: Vec::new(), unused: Vec::new(), random_deck: Vec::new() },
   };

   if app_config.times.is_empty() {
      app_config.panic_message("Config has to contain at least one time", MessageLevel::Critical);
   }

   //Check for images in image json
   let mut images = match load_images(&app_config, &mut not_used_images, None) {
      Ok(images) => images,
      Err(e) => {
         app_config.panic_message(&format!("Unable to load images.\nError: {:#}", e), MessageLevel::Error);
      }
   };
   save_images_ids(&mut not_used_images, &app_config);

   //Parse additional arguments
   for arg in args.iter().skip(2) {
      //Print image on start
      if arg == "--now" {
         let image = post_image(&app_config, &images, &mut not_used_images);
         if let Ok(image) = image {
            app_config.output_message(
               &format!("Image {} posted with --now at {}", image.location, Local::now()),
               MessageLevel::Info,
               MessageOutput::Stdout,
            );

            save_images_ids(&mut not_used_images, &app_config);
         }
      }
   }

   //Calculate next time for post and json refresh
   let current_time = Local::now();
   let mut next_time = get_next_time(current_time, &app_config);
   let mut image_config_refresh_time = Instant::now() + time::Duration::from_secs(60 * 60);

   app_config.output_message(&format!("Next image will be at {}", next_time), MessageLevel::Info, MessageOutput::Stdout);
   app_config.output_message(
      &format!("{}/{} images left", not_used_images.unused.len(), not_used_images.unused.len() + not_used_images.used.len()),
      MessageLevel::Info,
      MessageOutput::Stdout,
   );

   let mut failed_to_post = false;
   let mut failed_to_post_time = Instant::now();

   loop {
      //Check if there are changes in image json
      if image_config_refresh_time < Instant::now() {
         image_config_refresh_time = Instant::now() + time::Duration::from_secs(60 * 60); //Every hour
         images = match load_images(&app_config, &mut not_used_images, Some(&images)) {
            Ok(images_new) => images_new,
            Err(e) => {
               app_config.output_message(
                  &format!("Unable to load images, continuing with old json. Error:\n{:#}", e),
                  MessageLevel::Error,
                  MessageOutput::Stderr,
               );
               images //Returning old data
            }
         };

         save_images_ids(&mut not_used_images, &app_config);
      }

      //Check if it's time to post new image
      if next_time < Local::now()
         || (failed_to_post && (Instant::now() - failed_to_post_time).as_secs() > app_config.retry_time.unwrap_or(10 * 60))
      {
         //Try again after 10min if failed
         let image = post_image(&app_config, &images, &mut not_used_images);
         next_time = get_next_time(next_time, &app_config);

         if let Ok(image) = image {
            app_config.output_message(
               &format!("Image {} posted at {}, next at {}", image.location, Local::now(), next_time),
               MessageLevel::Info,
               MessageOutput::Stdout,
            );
            app_config.output_message(
               &format!("Image text: {}", image.msg.to_owned().unwrap_or_default()),
               MessageLevel::Info,
               MessageOutput::Stdout,
            );
            app_config.output_message(
               &format!("Image alt text: {}", image.alt.to_owned().unwrap_or_default()),
               MessageLevel::Info,
               MessageOutput::Stdout,
            );
            app_config.output_message(
               &format!(
                  "{}/{} images left",
                  not_used_images.unused.len(),
                  not_used_images.unused.len() + not_used_images.used.len()
               ),
               MessageLevel::Info,
               MessageOutput::Stdout,
            );

            failed_to_post = false;
            save_images_ids(&mut not_used_images, &app_config);
         } else {
            failed_to_post = true;
            failed_to_post_time = Instant::now();
         }
      }

      //Sleep till next check
      thread::sleep(time::Duration::from_secs(30));
   }
}
