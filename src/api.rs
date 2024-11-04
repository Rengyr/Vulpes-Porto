use std::{fs, path::Path};

use anyhow::{anyhow, Error, Result};
use reqwest::{
   blocking::{
      multipart::{self, Part},
      Client,
   },
   header::HeaderMap,
};
use serde_json::Value;

use crate::structures::{Config, Image, MessageLevel, MessageOutput, StatusVisibility};

static GITHUB_LINK: &str = "https://github.com/Rengyr/Vulpes-Porto";

/// Function to get a client with the correct headers
/// * `token` - Optional token to be used for authorization
pub fn get_client(token: Option<&str>) -> Result<Client, Error> {
   let mut headers = HeaderMap::new();
   if let Some(token) = token {
      headers.insert(
         reqwest::header::AUTHORIZATION,
         reqwest::header::HeaderValue::from_str(&("Bearer ".to_string() + token)).unwrap(),
      );
   }

   let client_media = match Client::builder()
      .user_agent("VulpesPorto/".to_string() + version!() + " (" + GITHUB_LINK + ")")
      .default_headers(headers)
      .build()
   {
      Ok(client_media) => client_media,
      Err(err) => {
         return Err(anyhow!(err).context("Unable to build client for request"));
      }
   };

   Ok(client_media)
}

/// Function to get json file with
/// * `sources_json_file_path` - Path to the json file with images
pub fn get_image_sources(sources_json_file_path: &str) -> Result<(String, Vec<Image>), Error> {
   let images_json = match Path::new(sources_json_file_path).exists() {
      // Images are local
      true => {
         // Allow to use "file:" prefix for local json file
         let image_json_path = sources_json_file_path.strip_prefix("file:").unwrap_or(sources_json_file_path);

         //Load the json file from disk
         match fs::read_to_string(image_json_path) {
            Ok(images_json) => images_json,
            Err(err) => return Err(anyhow!(err).context("Unable to read json file with images")),
         }
      }
      // Images are remote
      false => {
         // Build client for remote json file
         let client_media = match get_client(None) {
            Ok(client_media) => client_media,
            Err(err) => {
               return Err(anyhow!(err).context("Unable to make reqwest client for remote json file with images"));
            }
         };

         //Get json file from remote location
         let result = match client_media.get(sources_json_file_path).send() {
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

   Ok((images_json, images))
}

/// Function to upload image to media api
/// * `client` - Client to make requests
/// * `app_config` - Application configuration
/// * `image_bytes` - Bytes of the image
/// * `image` - Image structure
pub fn upload_image_to_media_api(
   client: &Client,
   app_config: &Config,
   image_bytes: Vec<u8>,
   image: &Image,
) -> Result<String, ()> {
   let part = Part::bytes(image_bytes).file_name("image");

   //Construct request to upload image to mastodon and get media id
   let mut media_request = multipart::Form::new()
      // Image
      .part("file", part);

   if let Some(alt) = image.alt.clone() {
      media_request = media_request.text("description", alt);
   }

   let response = client.post(app_config.server.to_owned() + "/api/v2/media").multipart(media_request).send();

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

   match media_json["id"].as_str() {
      Some(media_id) => Ok(media_id.to_string()),
      None => {
         app_config.output_message(
            &format!("Unable to get media id: {:?} for image {}", media_json, image.location),
            MessageLevel::Error,
            MessageOutput::Stderr,
         );
         Err(())
      }
   }
}

/// Function to make post with image on fedi
/// * `client` - Client to make requests
/// * `app_config` - Application configuration
/// * `media_id` - Media id of uploaded image from media api
/// * `image` - Image structure
pub fn create_new_status_with_image(client: &Client, app_config: &Config, media_id: String, image: &Image) -> Result<(), ()> {
   //Construct request to post new post to mastodon with the image
   let mut status_request = multipart::Form::new()
      // Image id
      .text("media_ids[]", media_id);

   if app_config.status_visibility != StatusVisibility::Default {
      status_request = status_request.text("visibility", app_config.status_visibility.to_string());
   }

   //Get the message on the image or default ""
   let mut message = image.msg.clone().unwrap_or_default();

   //If tags are specified then add tags after new line if message is not empty
   if !app_config.tags.is_empty() {
      if !message.is_empty() {
         message += "\n";
      }
      message += &app_config.tags;
   }

   //Add message to the posted image if there is something
   if !message.is_empty() {
      status_request = status_request.text("status", message);
   }

   //Add context warning to the posted image if there is something
   if let Some(content_warning) = &image.content_warning {
      status_request = status_request.text("spoiler_text", content_warning.to_owned());
   }

   let response = client.post(app_config.server.to_owned() + "/api/v1/statuses").multipart(status_request).send();

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

   Ok(())
}
