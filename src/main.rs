use core::time;
use std::{fs::{self, File}, env, thread, time::Instant, io::{Write, BufReader}, collections::HashMap};

use once_cell::sync::OnceCell;
use rand::{Rng};
use serde::{de::Error, Serialize, Deserialize, Deserializer};

use chrono::{Utc, DateTime, TimeZone, Local};

use reqwest::{blocking::multipart::{self, Part}};
use serde_json::Value;

use anyhow::{anyhow, Result};

static SYSTEMD_ERROR:OnceCell<String> = OnceCell::new();
static SYSTEMD_NOTICE:OnceCell<String> = OnceCell::new();
static SYSTEMD_INFO:OnceCell<String> = OnceCell::new();

///Structure holding configuration of the bot
#[derive(Serialize, Deserialize, Debug)]
struct Config {
    server: String,
    token: String,
    image_json: String,
    not_used_images_log_location: String,
    #[serde(deserialize_with = "from_string_time")]
    times: Vec<(u8,u8)>
}

fn from_string_time<'de, D>(deserializer: D) -> Result<Vec<(u8,u8)>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Vec<&str> = Deserialize::deserialize(deserializer)?;

    //Deserializing time to tuple with hours and minutes
    s.into_iter().map(|time| -> Result<(u8, u8), <D as Deserializer>::Error> {
        let mut time_split = time.split(':');
        let hours = time_split.next().ok_or_else(|| D::Error::custom("missing hours")).and_then(|h| h.parse::<u8>().map_err(|_| D::Error::custom("can't parse hours")));
        let minutes = time_split.next().ok_or_else(|| D::Error::custom("missing minutes")).and_then(|h| h.parse::<u8>().map_err(|_| D::Error::custom("can't parse minutes")));
        match (hours, minutes) {
            (Ok(hours), Ok(minutes)) => {
                if hours > 23{
                    Err(D::Error::custom("hours must be less than 23"))
                } else if minutes > 60 {
                    Err(D::Error::custom("minutes must be less than 60"))
                } else {
                    Ok((hours, minutes))
                }
            },
            (Err(hours), Ok(_)) => Err(hours),
            (Ok(_), Err(minutes)) => Err(minutes),
            _ => Err(D::Error::custom("invalid time"))
        }
    }).collect::<Result<Vec<(u8, u8)>, D::Error>>()
}

///Structure containing info about the image
#[derive(Serialize, Deserialize, Debug)]
struct Image {
    msg: Option<String>,    //Optional message
    alt: Option<String>,    //Optional alt text for image
    location: String,       //Link to hosted image
}

///Structure containing info about current used and unused images
#[derive(Serialize, Deserialize, Debug)]
struct ImageDB{
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
fn load_images(image_json_path: &str, images_db: &mut ImageDB, images_old: Option<&HashMap<String, Image>>) -> Result<HashMap<String, Image>> {
    //Get json file
    let result = match reqwest::blocking::get(image_json_path){
        Ok(result) => result,
        Err(err) => return Err(anyhow!(err).context("Unable to get images json"))
    };

    //Parse json as text
    let images_json = match result.text(){
        Ok(images_json) => images_json,
        Err(err) => return Err(anyhow!(err).context("Unable parse web result as text"))
    };

    //Parse json to images
    let images: Vec<Image> = match serde_json::from_str(&images_json){
        Ok(images) => images,
        Err(err) => return Err(anyhow!(err).context("Unable to parse text as images json"))
    };

    //Calculate md5 hashes as keys for images
    let images: HashMap<String, Image> = images.into_iter().map(|image| (format!("{:x}",md5::compute(&image.location)), image)).collect();

    //Add new images to unused list
    let mut new = 0;
    for hash in images.keys() {
        if !images_db.contains(hash){
            images_db.unused.push(hash.to_owned());
            new += 1;
        }
    }
    if new > 0 {
        println!("{}Added {} new images", SYSTEMD_NOTICE.get().unwrap_or(&"".to_string()), new);
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
    if removed > 0 {
        println!("{}Removed {} images not found in json", SYSTEMD_NOTICE.get().unwrap_or(&"".to_string()), removed);
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
        println!("{}Removed from random deck {} images not found in json", SYSTEMD_NOTICE.get().unwrap_or(&"".to_string()), removed_d);
    }

    //Check if alt text or text of images changed and write notice to stdout
    if let Some(images_old) = images_old{
        let mut message_changed = 0;
        let mut alt_changed = 0;
        for image in &images{
            if let Some(image_old) = images_old.get(image.0){
                if image_old.msg != image.1.msg{
                    message_changed += 1;
                }
                if image_old.alt != image.1.alt{
                    alt_changed += 1;
                }
            }
            if message_changed > 0 {
                println!("{}Text changed for {} images", SYSTEMD_NOTICE.get().unwrap_or(&"".to_string()), message_changed);
            }
            if alt_changed > 0 {
                println!("{}Alt text changed for {} images", SYSTEMD_NOTICE.get().unwrap_or(&"".to_string()), alt_changed);
            }
        }
    }

    Ok(images)
}

/// Return next closest time that is in the future given times in config or current time + 1 day if no times are configured.
/// 
/// Times in config has to be sorted.
#[allow(deprecated)]
fn get_next_time<Tz: TimeZone>(date_time: DateTime<Tz>, config: &Config) -> DateTime<Tz>{
    if config.times.is_empty() {
        return date_time + chrono::Duration::days(1);
    }

    let mut new_date_time = date_time.to_owned();
    let date_now:DateTime<Tz> = Utc::now().with_timezone(&date_time.timezone());

    //Loop until time is found
    loop {
        //Try all times in the config
        for (hours, minutes) in config.times.iter() {
            new_date_time = match new_date_time.date().and_hms_opt(hours.to_owned() as u32, minutes.to_owned() as u32, 0){
                Some(new_date_time) => new_date_time,
                None => {
                    panic!("{}Invalid hours or minutes in the configuration", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()))
                },
            };
            
            if date_now < new_date_time {
                return new_date_time;
            }
        }

        //Add one day if no time in config is in the future for current day
        new_date_time += chrono::Duration::days(1);
    }
}

///Send request for new media post to Mastodon server and return error is there is any.
fn post_image<'a>(app_config: &Config, images: &'a HashMap<String, Image>, images_db: &mut ImageDB) -> Result<&'a Image, ()> {
    let rng = &mut rand::thread_rng();

    //Get random hash from unused if there is any else from random deck
    let image_hash = match images_db.unused.is_empty() {
        true => {
            if images_db.random_deck.is_empty() {
                images_db.random_deck.append(&mut images_db.used.to_vec());
            }
            images_db.random_deck.get(rng.gen_range(0..images_db.random_deck.len())).unwrap().to_owned()
        }
        false => {
            images_db.unused.get(rng.gen_range(0..images_db.unused.len())).unwrap().to_owned()
        },
    };

    //Get image from hash
    let image = match images.get(&image_hash){
        Some(image) => image,
        None => {
            eprintln!("{}Can't find image with hash {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), image_hash);
            return Err(());
        },
    };

    //Make client for request
    let client_media = match reqwest::blocking::Client::builder()
        .user_agent("Vulpes Porto")
        .build(){
            Ok(client_media) => client_media,
            Err(e) => {
                eprintln!("{}Unable to make client for media request: {} for image {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), e, image.location);
                return Err(());
            },
        };

    //Download image to cache
    let response = match client_media.get(&image.location).send(){
        Ok(response) => response,
        Err(e) => {
            eprintln!("{}Unable to get image {}: {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), image.location, e);
            return Err(());
        },
    };

    let bytes = response;

    let part = Part::reader(bytes).file_name("image");

    let client = reqwest::blocking::Client::new();

    //Construct request to upload image to mastodon and get media id
    let mut media_request = multipart::Form::new()
        // Image
        .part("file", part);


    if let Some(alt) = image.alt.clone() {
        media_request = media_request.text("description", alt);
    }

    let response = client.post(app_config.server.to_owned() + "/api/v2/media")
       .header("Authorization", "Bearer ".to_string() + app_config.token.to_string().as_str())
       .multipart(media_request).send();
    
    let response = match response {
        Ok(response) => response,
        Err(e) => {
            eprintln!("{}Unable to post image to /api/v2/media: {} for image {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), e, image.location);
            return Err(());
        },
    };

    if !response.status().is_success() {
        if response.status() == 404{
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
                },
            };
        }
        eprintln!("{}Wrong status from media api: {} for image {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), response.status(), image.location);
        eprintln!("{}Response: {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), response.text().unwrap());
        return Err(());
    }

    let media_json: Value = match serde_json::from_str(&response.text().unwrap()) {
        Ok(media_json) => media_json,
        Err(e) => {
            eprintln!("{}Unable to parse media json: {} for image {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), e, image.location);
            return Err(());
        },
    };

    let media_id:String = match media_json["id"].as_str(){
        Some(media_id) => media_id.to_string(),
        None => {
            eprintln!("{}Unable to get media id: {:?} for image {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), media_json, image.location);
            return Err(());
        },
    };

    //Construct request to post new post to mastodon with the image
    let mut status_request = multipart::Form::new()
         // Image id
         .text("media_ids[]", media_id);

    if let Some(message) = image.msg.clone() {
        status_request = status_request.text("status", message);
    }
    
    let response = client.post(app_config.server.to_owned() + "/api/v1/statuses")
       .header("Authorization", "Bearer ".to_string() + app_config.token.to_string().as_str())
       .multipart(status_request).send();


    let response = match response {
        Ok(response) => response,
        Err(e) => {
            eprintln!("{}Unable to post image to /api/v1/statuses: {} for image {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), e, image.location);
            return Err(());
        },
    };
       
    if !response.status().is_success() {
        eprintln!("{}Wrong status from statuses api: {} for image {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), response.status(), image.location);
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
        },
    };

    Ok(image)
}

///Save used and unused images to file.
fn save_images_ids(image_db: &mut ImageDB, app_config: &Config) {
    match File::create(app_config.not_used_images_log_location.clone()){
        Ok(mut file) => {
            file.write_all(serde_json::to_string(&image_db).unwrap().as_bytes()).unwrap();
        },
        Err(e) => {
            eprintln!("{}Unable to create not_used_images_log_location: {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), e);
        },
    };
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        println!("Usage: {} <config.json> [--now] [--systemd]", args[0]);
        return;
    }

    //Parse systemd first to use it for further messages
    for arg in args.iter().skip(2){
        //Set systemd output style
        if arg == "--systemd" {
            let _ = SYSTEMD_ERROR.set("<3>".to_string());
            let _ = SYSTEMD_NOTICE.set("<5>".to_string());
            let _ = SYSTEMD_INFO.set("<6>".to_string());
        }
    }

    //Set default value if it wasn't set before, we don't care if it fails from being already set
    let _ = SYSTEMD_ERROR.set("".to_string());
    let _ = SYSTEMD_NOTICE.set("".to_string());
    let _ = SYSTEMD_INFO.set("".to_string());
    
    //Load bot configuration
    let app_config = fs::read_to_string(&args[1]).unwrap_or_else(|_| panic!("{}Couldn't find config.json file", SYSTEMD_ERROR.get().unwrap_or(&"".to_string())));

    let mut app_config: Config = serde_json::from_str(&app_config).unwrap_or_else(|_| panic!("{}Unable to parse config.json", SYSTEMD_ERROR.get().unwrap_or(&"".to_string())));
    app_config.times.sort_unstable();

    //Load used and unused list of images
    let mut not_used_images = match File::open(app_config.not_used_images_log_location.clone()){
        Ok(file) => {
            let reader = BufReader::new(file);
            match serde_json::from_reader(reader){
                Ok(res) => res,
                Err(e) => {
                    panic!("{}Unable to parse not_used_images_log: {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()),e);
                },
            }
        },
        Err(_) => {
            ImageDB{
                used: Vec::new(),
                unused: Vec::new(),
                random_deck: Vec::new(),
            }
        }
        
    };

    if app_config.times.is_empty() {
        panic!("{}Config has to contain at least one time",SYSTEMD_ERROR.get().unwrap_or(&"".to_string()));
    }

    //Check for images in image json
    let mut images = match load_images(&app_config.image_json, &mut not_used_images, None){
        Ok(images) => images,
        Err(e) => {
            panic!("{}Unable to load images: {}", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), e);
        },
    };


    //Parse additional arguments
    for arg in args.iter().skip(2){
        //Print image on start
        if arg == "--now" {
            let image = post_image(&app_config, &images, &mut not_used_images);
            if let Ok(image) = image {
                println!("{}Image {} posted with --now at {}", SYSTEMD_NOTICE.get().unwrap_or(&"".to_string()), image.location, Local::now());

                save_images_ids(&mut not_used_images, &app_config);
            }
        }
    }
   
    //Calculate next time for post and json refresh
    let current_time = Local::now();
    let mut next_time = get_next_time(current_time, &app_config);
    let mut image_config_refresh_time = Instant::now() + time::Duration::from_secs(60*60);

    println!("{}Next image will be at {}", SYSTEMD_NOTICE.get().unwrap_or(&"".to_string()), next_time);
    println!("{}{}/{} images left", SYSTEMD_NOTICE.get().unwrap_or(&"".to_string()), not_used_images.unused.len(), not_used_images.unused.len() + not_used_images.used.len());

    let mut failed_to_post = false;
    let mut failed_to_post_time = Instant::now();

    loop {
        //Check if there are changes in image json
        if image_config_refresh_time < Instant::now() {
            image_config_refresh_time = Instant::now() + time::Duration::from_secs(60*60);  //Every hour
            images = match load_images(&app_config.image_json, &mut not_used_images, Some(&images)){
                Ok(images_new) => {
                    images_new
                }
                Err(e) => {
                    eprintln!("{}Unable to load images: {}, continuing with old json", SYSTEMD_ERROR.get().unwrap_or(&"".to_string()), e);
                    images  //Returning old data
                },
            };

            save_images_ids(&mut not_used_images, &app_config);
        }

        //Check if it's time to post new image
        if next_time < Local::now() || (failed_to_post && (Instant::now() - failed_to_post_time).as_secs() > 60*10){    //Try again after 10min if failed
            let image = post_image(&app_config, &images, &mut not_used_images);
            next_time = get_next_time(next_time, &app_config);

            if let Ok(image) = image {
                println!("{}Image {} posted at {}, next at {}", SYSTEMD_NOTICE.get().unwrap_or(&"".to_string()), image.location, Local::now(), next_time);
                println!("{}Image text: {}", SYSTEMD_INFO.get().unwrap_or(&"".to_string()), image.msg.to_owned().unwrap_or_default());
                println!("{}Image alt text: {}", SYSTEMD_INFO.get().unwrap_or(&"".to_string()), image.alt.to_owned().unwrap_or_default());
                println!("{}{}/{} images left", SYSTEMD_NOTICE.get().unwrap_or(&"".to_string()), not_used_images.unused.len(), not_used_images.unused.len() + not_used_images.used.len());

                failed_to_post = false;
                save_images_ids(&mut not_used_images, &app_config);
            }
            else{
                failed_to_post = true;
                failed_to_post_time = Instant::now();
            }
            
        }
        
        //Sleep till next check
        thread::sleep(time::Duration::from_secs(30));
    }
    
}
