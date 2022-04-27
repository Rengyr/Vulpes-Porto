use core::time;
use std::{fs::{self, File}, env, thread, time::Instant, io::{Write, BufReader}};

use rand::{Rng};
use serde::{de::Error, Serialize, Deserialize, Deserializer};

use chrono::{Utc, DateTime, TimeZone, Local};

use reqwest::blocking::multipart::{self, Part};
use serde_json::Value;

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

#[derive(Serialize, Deserialize, Debug)]
struct Image {
    msg: Option<String>,
    location: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct ImagesLeft{
    total_amount: usize,
    unused: Vec<usize>,
}

fn load_images(image_json_path: &str, not_used_images: &mut ImagesLeft) -> Vec<Image> {
    let images_json = reqwest::blocking::get(image_json_path).expect("Unable to get images.json").text().expect("Unable parse images.json as text");
    let images: Vec<Image> = serde_json::from_str(&images_json).unwrap();

    if not_used_images.total_amount < images.len() {
        for i in not_used_images.total_amount..images.len() {
            not_used_images.unused.push(i);
        }
    }
    if not_used_images.total_amount > images.len() {
        eprintln!("Config has fewer images than before, resetting not_used_images");
        not_used_images.unused.clear();
        for i in 0..images.len() {
            not_used_images.unused.push(i);
        }
    }

    not_used_images.total_amount = images.len();

    images
}

/// Return next closest time that is in the future given times in config.
/// 
/// Expect times in config to be sorted.
fn get_next_time<Tz: TimeZone>(date_time: DateTime<Tz>, config: &Config) -> DateTime<Tz>{
    let mut new_date_time = date_time.to_owned();
    let date_now:DateTime<Tz> = Utc::now().with_timezone(&date_time.timezone());

    loop {
        for (hours, minutes) in config.times.iter() {
            new_date_time = new_date_time.date().and_hms(*hours as u32,*minutes as u32, 0);

            if date_now < new_date_time {
                return new_date_time;
            }
        }

        new_date_time = new_date_time + chrono::Duration::days(1);
    }
}

fn post_image(app_config: &Config, images: &[Image], not_used_images: &mut ImagesLeft){
    let rng = &mut rand::thread_rng();
    let image_id = match not_used_images.unused.is_empty() {
        true => rng.gen_range(0..images.len()) as usize,
        false => {
            not_used_images.unused.remove(rng.gen_range(0..not_used_images.unused.len()))
        },
    };

    let image = match reqwest::blocking::get(images[image_id].location.to_owned()){
        Ok(image) => image,
        Err(e) => {
            eprintln!("Unable to get image {}: {}", images[image_id].location, e);
            return;
        },
    };

    let bytes = image;

    let part = Part::reader(bytes).file_name("image");

    let client = reqwest::blocking::Client::new();

    let media_request = multipart::Form::new()
        // Image
        .part("file", part);

    let response = client.post(app_config.server.to_owned() + "/api/v1/media")
       .header("Authorization", "Bearer ".to_string() + app_config.token.to_string().as_str())
       .multipart(media_request).send();
    
    let response = match response {
        Ok(response) => response,
        Err(e) => {
            eprintln!("Unable to post image to /api/v1/media: {}", e);
            return;
        },
    };

    let media_json: Value = match serde_json::from_str(&response.text().unwrap()) {
        Ok(media_json) => media_json,
        Err(e) => {
            eprintln!("Unable to parse media json: {}", e);
            return;
        },
    };

    let media_id:String = match media_json["id"].as_str(){
        Some(media_id) => media_id.to_string(),
        None => {
            eprintln!("Unable to get media id");
            return;
        },
    };

    let mut status_request = multipart::Form::new()
         // Image id
         .text("media_ids[]", media_id);

    if let Some(message) = images[image_id].msg.clone() {
        status_request = status_request.text("status", message);
    }
    
    let response = client.post(app_config.server.to_owned() + "/api/v1/statuses")
       .header("Authorization", "Bearer ".to_string() + app_config.token.to_string().as_str())
       .multipart(status_request).send();


    if let Err(e) = response {
        eprintln!("Unable to post image to /api/v1/media: {}", e);
    };
}

fn save_unused_images_ids(not_used_images: &mut ImagesLeft, app_config: &Config) {
    match File::create(app_config.not_used_images_log_location.clone()){
        Ok(mut file) => {
            file.write_all(serde_json::to_string(&not_used_images).unwrap().as_bytes()).unwrap();
        },
        Err(e) => {
            eprintln!("Unable to create not_used_images_log_location: {}", e);
        },
    };
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        println!("Usage: {} <config.json>", args[0]);
        return;
    }

    let app_config = fs::read_to_string(&args[1]).expect("Couldn't find config.json file");

    let mut app_config: Config = serde_json::from_str(&app_config).expect("Unable to parse config.json");
    app_config.times.sort_unstable();

    let mut not_used_images = match File::open(app_config.not_used_images_log_location.clone()){
        Ok(file) => {
            let reader = BufReader::new(file);
            match serde_json::from_reader(reader){
                Ok(res) => res,
                Err(e) => {
                    eprintln!("Unable to parse not_used_images_log: {}", e);
                    ImagesLeft{
                        total_amount: 0,
                        unused: Vec::new(),
                    }
                },
            }
        },
        Err(_) => {
            ImagesLeft{
                total_amount: 0,
                unused: Vec::new(),
            }
        }
        
    };

    let mut images = load_images(&app_config.image_json, &mut not_used_images);

    save_unused_images_ids(&mut not_used_images, &app_config);

    let current_time = Local::today().and_hms(0, 0, 0);
    let mut next_time = get_next_time(current_time, &app_config);
    let mut image_config_refresh_time = Instant::now() + time::Duration::from_secs(60*60*12);


    loop {
        if image_config_refresh_time < Instant::now() {
            image_config_refresh_time = Instant::now() + time::Duration::from_secs(60*60*12);
            images = load_images(&app_config.image_json, &mut not_used_images);

            save_unused_images_ids(&mut not_used_images, &app_config);
        }

        if next_time < Local::now() {
            post_image(&app_config, &images, &mut not_used_images);
            next_time = get_next_time(next_time, &app_config);

            println!("Posted image at {}, next at {}", Local::now(), next_time);

            save_unused_images_ids(&mut not_used_images, &app_config);
        }
        
        thread::sleep(time::Duration::from_secs(30));
    }

}
