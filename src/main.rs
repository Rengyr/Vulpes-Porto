#[macro_use]
extern crate version;

mod api;
mod structures;

use api::{create_new_status_with_image, get_client, get_image_sources, upload_image_to_media_api};
use clap::{arg, command, CommandFactory, Parser};
use structures::{save_images_ids, Config, GetImageErrorLevel, Image, ImageDB, MessageLevel, MessageOutput, StatusVisibility};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, NaiveTime, TimeZone, Utc};
use core::time;
use rand::Rng;
use std::{
   collections::{HashMap, HashSet}, fs::{File}, io::{BufReader, Read}, path::Path, process::exit, sync::{atomic::AtomicBool, Arc}, thread, time::Instant
};

/// From link to json load new image and parse the results to ImageDB structure. Returns Hashmap with images with keys of md5 hashes or returns Error.
/// * `app_config` - Configuration of the bot
/// * `images_db` - Structure holding used and unused images
/// * `images_old` - Old images to check if there are changes
fn load_image_paths(
   app_config: &Config,
   images_db: &mut ImageDB,
   images_old: Option<&HashMap<String, Image>>,
) -> Result<HashMap<String, Image>> {
   let (images_json, parsed_images) = get_image_sources(&app_config.get_image_json_path())?;

   report_duplicate_source_image_locations(app_config, &images_json, &parsed_images);

   //Calculate md5 hashes as keys for images
   let images: HashMap<String, Image> = parsed_images.into_iter().map(|image| (image.get_hash(), image)).collect();

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
   let total = images_db.unused.len() + images_db.used.len();
   images_db.unused.retain(|hash| images.contains_key(hash));
   images_db.used.retain(|hash| images.contains_key(hash));
   let removed = total - images_db.unused.len() - images_db.used.len();
   if removed > 0 {
      app_config.output_message(
         &format!("Removed {} images not found in json", removed),
         MessageLevel::Notice,
         MessageOutput::Stdout,
      );
   }

   //Remove images that were removed from json from random deck
   let total_deck = images_db.random_deck.len();
   images_db.random_deck.retain(|hash| images.contains_key(hash));
   let removed_d = total_deck - images_db.random_deck.len();
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

/// Reports images in sources file with same location
/// * `app_config` - Configuration of the bot
/// * `images_json` - JSON string with images for reporting actual line in file
/// * `parsed_images` - Parsed images to check for duplicity
fn report_duplicate_source_image_locations(app_config: &Config, images_json: &str, parsed_images: &[Image]) {
   //Calculate md5 hashes as keys for duplicity check
   let images_hashes: Vec<(String, String)> =
      parsed_images.iter().map(|image| (image.get_hash(), image.location.clone())).collect();

   // Keep list of reported duplicates to avoid duplicate warnings
   let mut reported_duplicates = HashSet::new();
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

            reported_duplicates.insert(index_duplicate);

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
}

/// Return next closest time that is in the future given times in config or current time + 1 day if no times are configured.
///
/// **Times in config has to be sorted**
#[allow(deprecated)]
fn get_next_post_time<Tz: TimeZone>(date_time: DateTime<Tz>, config: &Config) -> DateTime<Tz> {
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

/// Get image data from local or remote based on the image path
/// * `local_path` - Path to root folder
/// * `image_path` - Path to the image from root folder
fn get_image_data(local_path: Option<&String>, image_path: &str) -> Result<Vec<u8>, GetImageErrorLevel> {
   if let Some(image_path) = image_path.strip_prefix("file:") {
      // Fetch local image
      let Some(local_path) = local_path else {
         return Err(GetImageErrorLevel::Critical(anyhow!("Missing local path in configuration file")));
      };
      get_image_data_local(Path::new(local_path), Path::new(image_path))
   } else {
      // Fetch remote image
      get_image_data_remote(image_path)
   }
}

/// Read and parse image data from local path
/// * `local_path` - Path to root folder
/// * `image_path` - Path to the image from root folder
fn get_image_data_local(local_path: &Path, image_path: &Path) -> Result<Vec<u8>, GetImageErrorLevel> {
   let path = local_path.join(image_path);
   // Check if path exist
   if !path.exists() {
      return Err(GetImageErrorLevel::Critical(anyhow!("Image at local file '{}' does not exist", path.display())));
   }

   // Directory traversal attack mitigation
   let path = match path.canonicalize() {
      Ok(path) => path,
      Err(error) => {
         return Err(GetImageErrorLevel::Critical(anyhow!(
            "Can't make canonical absolute path for image {}: {:#}",
            image_path.display(),
            error
         )))
      }
   };
   let local_canon_path = match local_path.canonicalize() {
      Ok(path) => path,
      Err(error) => {
         return Err(GetImageErrorLevel::Critical(anyhow!(
            "Can't make canonical absolute path for local path {}: {:#}",
            local_path.display(),
            error
         )))
      }
   };
   if !path.starts_with(local_canon_path) {
      return Err(GetImageErrorLevel::Critical(anyhow!(
         "Directory traversal is not permitted for local image {}",
         image_path.display()
      )));
   }

   // Read file
   let mut bytes: Vec<u8> = Vec::new();
   let mut file = match File::open(path) {
      Ok(file) => file,
      Err(error) => return Err(GetImageErrorLevel::Critical(anyhow!("Can't open image {}: {:#}", image_path.display(), error))),
   };

   match file.read_to_end(&mut bytes) {
      Ok(_) => {}
      Err(error) => {
         return Err(GetImageErrorLevel::Normal(anyhow!("Error during reading image {}: {:+}", image_path.display(), error)));
      }
   };

   Ok(bytes)
}

/// Read and parse image data from remote path
/// * `remote_image_path` - Path to the image
fn get_image_data_remote(remote_image_path: &str) -> Result<Vec<u8>, GetImageErrorLevel> {
   //Make client for request
   let client_media = match get_client(None) {
      Ok(client_media) => client_media,
      Err(e) => {
         return Err(GetImageErrorLevel::Normal(anyhow!("Unable to initialize client to fetch remote images: {:#}", e)));
      }
   };

   //Download image to cache
   let response = match client_media.get(remote_image_path).send() {
      Ok(response) => response,
      Err(e) => {
         return Err(GetImageErrorLevel::Normal(anyhow!("Unable to get remote image {}: {:#}", remote_image_path, e)));
      }
   };

   if response.status() == 401 || response.status() == 403 || response.status() == 404 {
      return Err(GetImageErrorLevel::Critical(anyhow!(
         "Client error response when getting remote image {}: {}",
         remote_image_path,
         response.status()
      )));
   }

   match response.bytes() {
      Ok(bytes) => Ok(bytes.into_iter().collect()),
      Err(error) => Err(GetImageErrorLevel::Normal(anyhow!(
         "Response from remote image {} request is wrong: {:#}",
         remote_image_path,
         error
      ))),
   }
}

/// Send request for new media post to the server and return error if there is any
/// * `app_config` - Configuration of the bot
/// * `images` - Hashmap with images
/// * `internal_db` - Database of images
fn post_image<'a>(app_config: &Config, images: &'a HashMap<String, Image>, internal_db: &mut ImageDB) -> Result<&'a Image, ()> {
   let image = get_image_to_post(app_config, images, internal_db)?;
   let image_hash = image.get_hash();

   let image_bytes = get_image_data(app_config.local_path.as_ref(), &image.location);

   // Check if image data was fetched correctly
   let Ok(image_bytes) = image_bytes else {
      let error = image_bytes.unwrap_err();
      let error_message = match &error {
         GetImageErrorLevel::Normal(message) => message,
         GetImageErrorLevel::Critical(message) => message,
      };
      app_config.output_message(&format!("{:#}", error_message), MessageLevel::Error, MessageOutput::Stderr);

      //Remove image for critical errors
      if matches!(error, GetImageErrorLevel::Critical { .. }) {
         match internal_db.unused.is_empty() {
            true => {
               let pos = internal_db.random_deck.iter().position(|hash| hash == &image_hash).unwrap();
               internal_db.random_deck.remove(pos);
            }
            false => {
               let pos = internal_db.unused.iter().position(|hash| hash == &image_hash).unwrap();
               internal_db.unused.remove(pos);
               internal_db.used.push(image_hash.to_owned());
            }
         };
      }
      return Err(());
   };

   let client = match get_client(Some(&app_config.token)) {
      Ok(client) => client,
      Err(e) => {
         app_config.output_message(
            &format!("Unable to initialize client to post image: {:#}", e),
            MessageLevel::Error,
            MessageOutput::Stderr,
         );
         return Err(());
      }
   };

   let media_id: String = upload_image_to_media_api(&client, app_config, image_bytes, image)?;

   let (status_visiblity, new_vis_sequence) = get_status_visibility(app_config, internal_db);

   create_new_status_with_image(&client, app_config, media_id, image, status_visiblity)?;

   //Remove hash from the lists
   match internal_db.unused.is_empty() {
      true => {
         let pos = internal_db.random_deck.iter().position(|hash| hash == &image_hash).unwrap();
         internal_db.random_deck.remove(pos);
      }
      false => {
         let pos = internal_db.unused.iter().position(|hash| hash == &image_hash).unwrap();
         internal_db.unused.remove(pos);
         internal_db.used.push(image_hash.to_owned());
      }
   };

   internal_db.visiblity_sequence = new_vis_sequence;

   Ok(image)
}

/// Select image to post from database of images
/// * `app_config` - Configuration of the bot
/// * `images` - Hashmap with images
/// * `images_db` - Database of images
fn get_image_to_post<'a>(
   app_config: &Config,
   images: &'a HashMap<String, Image>,
   images_db: &mut ImageDB,
) -> Result<&'a Image, ()> {
   if images_db.used.is_empty() && images_db.unused.is_empty() {
      app_config.panic_message("No image to post contained in image_json file", MessageLevel::Critical);
   }

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
   match images.get(&image_hash) {
      Some(image) => Ok(image),
      None => {
         app_config.output_message(
            &format!("Can't find image with hash {}", image_hash),
            MessageLevel::Error,
            MessageOutput::Stderr,
         );
         Err(())
      }
   }
}

/// Get status visibility based on the configuration and internal database
/// * `app_config` - Configuration of the bot
/// * `internal_db` - Database of images
///
/// Returns tuple with status visibility and new visibility sequence if posting succeeded
fn get_status_visibility(app_config: &Config, internal_db: &ImageDB) -> (StatusVisibility, usize) {
   match &app_config.status_visibility_sequence {
      None => (app_config.status_visibility.clone(), 0),
      Some(sequence) => {
         let visibility = sequence[internal_db.visiblity_sequence % sequence.len()].clone();
         let new_sequence_number = (internal_db.visiblity_sequence + 1) % sequence.len();
         (visibility, new_sequence_number)
      }
   }
}

#[derive(Parser, Debug)]
#[command(version, about = "Mastodon bot to post remotely hosted photos daily at set times")]
struct Args {
   #[arg(short, long, help = "Path to the configuration file")]
   config: Option<String>,

   #[arg(short, long, action, help = "Post one image immediately at start and then follow schedule")]
   now: bool,

   #[arg(long, action, help = "Use systemd output style (deprecated)", hide = true)]
   systemd: bool,

   #[arg(short = 'C', long, help = "Test if the configuration and images are correct", action)]
   check: bool,

   #[arg(trailing_var_arg = true, hide = true)]
   config_old: Vec<String>,
}

fn main() {
   let args = Args::parse();

   if args.config.is_some() && !args.config_old.is_empty() {
      eprintln!("You can't use both --config and positional argument for config file\n");
      let mut cmd = Args::command();
      let _ = cmd.print_help();
      exit(1);
   }

   if args.config_old.len() > 1 {
      eprintln!("You can't use multiple positional arguments for config file\n");
      let mut cmd = Args::command();
      let _ = cmd.print_help();
      exit(1);
   }

   if args.config.is_none() && args.config_old.is_empty() {
      eprintln!("You have to provide config file\n");
      let mut cmd = Args::command();
      let _ = cmd.print_help();
      exit(1);
   }

   let config_path =
      args.config.unwrap_or_else(|| args.config_old.first().expect("Precondition were removed from code?").to_string());

   //Load bot configuration
   let config_file = config::Config::builder().add_source(config::File::with_name(&config_path)).build();

   let mut app_config: Config = match config_file {
      Ok(config) => {
         match config.try_deserialize() {
            Ok(config) => config,
            Err(e) => {
               eprintln!("Unable to parse configuration file.\nError: {:#}", e);
               exit(1);
            }
         }
      }
      Err(e) => {
         eprintln!("Unable to load configuration file.\nError: {:#}", e);
         exit(1);
      }
   };

   app_config.times.sort_unstable();

   app_config.config_path = config_path;

   if args.systemd {
      app_config.use_syslog_style = Some(true);
      app_config.output_message(
         "Using --systemd is deprecated, use setting in configuration file instead",
         MessageLevel::Notice,
         MessageOutput::Stdout,
      );
   }

   //Load used and unused list of images
   let mut internal_db = match File::open(app_config.get_internal_database_path()) {
      Ok(file) => {
         let reader = BufReader::new(file);
         match serde_json::from_reader(reader) {
            Ok(res) => res,
            Err(e) => {
               app_config
                  .panic_message(&format!("Unable to parse internal_database file.\nError: {:#}", e), MessageLevel::Critical);
            }
         }
      }
      Err(_) => ImageDB { used: Vec::new(), unused: Vec::new(), random_deck: Vec::new(), visiblity_sequence: 0 },
   };

   if app_config.times.is_empty() {
      app_config.panic_message("Config has to contain at least one post time", MessageLevel::Critical);
   }

   //Check for images in image json
   let mut images = match load_image_paths(&app_config, &mut internal_db, None) {
      Ok(images) => images,
      Err(e) => {
         app_config.panic_message(&format!("Unable to load images.\nError: {:#}", e), MessageLevel::Error);
      }
   };

   // Run checks
   let check = api::check_connection(&app_config);
   if args.check {
      app_config.output_message("Configuration and images are correct", MessageLevel::Info, MessageOutput::Stdout);
      match check {
         Ok(info) => {
            if let Some(info) = info {
               app_config.output_message(&info, MessageLevel::Info, MessageOutput::Stdout)
            }
            exit(0);
         }
         Err(error) => app_config.panic_message(&error, MessageLevel::Critical),
      }
   } else {
      match check {
         Ok(info) => {
            if let Some(info) = info {
               app_config.output_message(&info, MessageLevel::Info, MessageOutput::Stdout)
            }
         }
         Err(error) => app_config.output_message(&error, MessageLevel::Critical, MessageOutput::Stderr),
      }
   }

   save_images_ids(&mut internal_db, &app_config);

   if args.now {
      let image = post_image(&app_config, &images, &mut internal_db);
      if let Ok(image) = image {
         app_config.output_message(
            &format!("Image {} posted with --now at {}", image.location, Local::now()),
            MessageLevel::Info,
            MessageOutput::Stdout,
         );

         save_images_ids(&mut internal_db, &app_config);
      }
   }

   // Register handler for SIGUSR1 signal to reload config on Unix systems
   let reload_signal = Arc::new(AtomicBool::new(false));
   #[cfg(not(windows))]
   {
      if let Err(error) = signal_hook::flag::register(signal_hook::consts::SIGUSR1, Arc::clone(&reload_signal))
      {
         app_config.output_message(&format!("Unable to register signal handler for config reload: {:#}", error), MessageLevel::Error, MessageOutput::Stderr);
      }
   }

   //Calculate next time for post and json refresh
   let current_time = Local::now();
   let mut next_time = get_next_post_time(current_time, &app_config);
   let mut image_config_refresh_time = Instant::now() + time::Duration::from_secs(60 * 30);

   app_config.output_message(&format!("Next image will be at {}", next_time), MessageLevel::Info, MessageOutput::Stdout);
   app_config.output_message(
      &format!("{}/{} images left", internal_db.unused.len(), internal_db.unused.len() + internal_db.used.len()),
      MessageLevel::Info,
      MessageOutput::Stdout,
   );

   let mut failed_to_post = false;
   let mut failed_to_post_time = Instant::now();

   loop {
      //Check if there are changes in image json
      if image_config_refresh_time < Instant::now() || reload_signal.load(std::sync::atomic::Ordering::Relaxed) {
         image_config_refresh_time = Instant::now() + time::Duration::from_secs(60 * 30); // Reload images every 30 minutes
         reload_signal.store(false, std::sync::atomic::Ordering::Relaxed);
         images = match load_image_paths(&app_config, &mut internal_db, Some(&images)) {
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

         save_images_ids(&mut internal_db, &app_config);
      }

      //Check if it's time to post new image or retry posting image
      if next_time < Local::now() || (failed_to_post && (Instant::now() - failed_to_post_time).as_secs() > app_config.retry_time)
      {
         let image = post_image(&app_config, &images, &mut internal_db);
         next_time = get_next_post_time(next_time, &app_config);

         if let Ok(image) = image {
            app_config.output_message(
               &format!("Image {} posted at {}, next at {}", image.location, Local::now(), next_time),
               MessageLevel::Info,
               MessageOutput::Stdout,
            );
            app_config.output_message(
               &format!("Image text: {}", image.msg.as_deref().unwrap_or_default()),
               MessageLevel::Info,
               MessageOutput::Stdout,
            );
            app_config.output_message(
               &format!("Image alt text: {}", image.alt.as_deref().unwrap_or_default()),
               MessageLevel::Info,
               MessageOutput::Stdout,
            );
            app_config.output_message(
               &format!("{}/{} images left", internal_db.unused.len(), internal_db.unused.len() + internal_db.used.len()),
               MessageLevel::Info,
               MessageOutput::Stdout,
            );

            failed_to_post = false;
            save_images_ids(&mut internal_db, &app_config);
         } else {
            failed_to_post = true;
            failed_to_post_time = Instant::now();
         }
      }

      //Sleep till next check
      thread::sleep(time::Duration::from_secs(30));
   }
}
