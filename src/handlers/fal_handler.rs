use crate::twitch_stream_state;
// use anyhow::anyhow;
use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use events::EventHandler;
use fal_ai;
use obws::Client as OBSClient;
use regex::Regex;
use rodio::*;
use serde::Deserialize;
// use std::io::Write;
//use reqwest::Client;
//use serde_json::json;
//use std::path::Path;
// use tracing_subscriber::registry::SpanData;
use subd_types::{Event, UserMessage};
use tokio::time::{sleep, Duration};

// Which do I need?
// use std::fs::File;
use tokio::fs::File;
// use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;

use fal_rust::client::{ClientCredentials, FalClient};
use twitch_irc::{
    login::StaticLoginCredentials, SecureTCPTransport, TwitchIRCClient,
};

#[derive(Deserialize)]
struct FalImage {
    url: String,
    _width: Option<u32>,
    _height: Option<u32>,
    _content_type: Option<String>,
}

#[derive(Deserialize)]
struct FalData {
    images: Vec<FalImage>,
    // Other fields can be added here if needed
}

pub struct FalHandler {
    // pub queue_rx: &'static queue::SourcesQueueOutput<f32>,
    pub obs_client: OBSClient,
    pub pool: sqlx::PgPool,
    pub sink: Sink,
    pub twitch_client:
        TwitchIRCClient<SecureTCPTransport, StaticLoginCredentials>,
}

#[async_trait]
impl EventHandler for FalHandler {
    async fn handle(
        self: Box<Self>,
        tx: broadcast::Sender<Event>,
        mut rx: broadcast::Receiver<Event>,
    ) -> Result<()> {
        loop {
            let event = rx.recv().await?;
            let msg = match event {
                Event::UserMessage(msg) => msg,
                _ => continue,
            };

            let splitmsg = msg
                .contents
                .split(" ")
                .map(|s| s.to_string())
                .collect::<Vec<String>>();

            match handle_fal_commands(
                &tx,
                &self.obs_client,
                &self.twitch_client,
                &self.pool,
                &self.sink,
                splitmsg,
                msg,
            )
            .await
            {
                Ok(_) => {}
                Err(err) => {
                    eprintln!("Error: {err}");
                    continue;
                }
            }
        }
    }
}

pub async fn handle_fal_commands(
    _tx: &broadcast::Sender<Event>,
    obs_client: &OBSClient,
    _twitch_client: &TwitchIRCClient<
        SecureTCPTransport,
        StaticLoginCredentials,
    >,
    pool: &sqlx::PgPool,
    _sink: &Sink,
    splitmsg: Vec<String>,
    msg: UserMessage,
) -> Result<()> {
    //let default_source = constants::DEFAULT_SOURCE.to_string();
    // let source: &str = splitmsg.get(1).unwrap_or(&default_source);

    let is_mod = msg.roles.is_twitch_mod();
    let _not_beginbot =
        msg.user_name != "beginbot" && msg.user_name != "beginbotbot";
    let command = splitmsg[0].as_str();
    let word_count = msg.contents.split_whitespace().count();

    match command {
        "!theme" => {
            if _not_beginbot && !is_mod {
                return Ok(());
            }
            let theme = &splitmsg
                .iter()
                .skip(1)
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>()
                .join(" ");
            twitch_stream_state::set_ai_background_theme(pool, &theme).await?;
        }

        "!talk" => {
            println!("\n\nTALK TIME!");
            let image_file_path = "teej_2.jpg";
            fal_ai::create_video_from_image(image_file_path).await;
            // let fal_image_file_path = "green_prime.png";
            // let fal_audio_file_path =
            //     "TwitchChatTTSRecordings/1700109062_siifr_neo.wav";
            //
            // let video_bytes = fal_ai::sync_lips_to_voice(
            //     fal_image_file_path,
            //     fal_audio_file_path,
            // )
            // .await?;
            //
            // let video_path = "./prime.mp4";
            // tokio::fs::write(&video_path, &video_bytes).await?;
            // println!("Video saved to {}", video_path);
            //
            // let scene = "Primary";
            // let source = "prime-talking-video";
            // let _ = crate::obs::obs_source::set_enabled(
            //     scene, source, false, obs_client,
            // )
            // .await;
            //
            // // Not sure if I have to wait ofr how long to wait
            // sleep(Duration::from_millis(100)).await;
            //
            // let _ = crate::obs::obs_source::set_enabled(
            //     scene, source, true, obs_client,
            // )
            // .await;
        }

        "!fal" => {}

        _ => {
            if !command.starts_with('!')
                && !command.starts_with('@')
                && word_count > 1
            {
                // Get the user's message content as the prompt
                let prompt = msg.contents;

                // Retrieve the current AI background theme from the database
                let theme =
                    twitch_stream_state::get_ai_background_theme(pool).await?;

                // Combine the theme and user's prompt to create the final prompt
                let final_prompt = format!("{} {}", theme, prompt);

                println!("Final Prompt for BG: {}", final_prompt);

                // Generate an image using the final prompt with the Fal AI service
                fal_ai::create_turbo_image(final_prompt).await?;

                // let theme = "Waifu";
                // let final_prompt = format!("{} {}", theme, prompt);
                // create_turbo_image(final_prompt).await?;
            }
        }
    };

    Ok(())
}

async fn process_images(
    timestamp: &str,
    json_path: &str,
    extra_save_folder: Option<&str>,
) -> Result<()> {
    // Read the JSON file asynchronously
    let json_data = tokio::fs::read_to_string(json_path).await?;

    // Parse the JSON data into the FalData struct
    let data: FalData = serde_json::from_str(&json_data)?;

    // Regex to match data URLs
    let data_url_regex =
        Regex::new(r"data:(?P<mime>[\w/]+);base64,(?P<data>.+)")?;

    for (index, image) in data.images.iter().enumerate() {
        // Match the data URL and extract MIME type and base64 data
        if let Some(captures) = data_url_regex.captures(&image.url) {
            let mime_type = captures.name("mime").unwrap().as_str();
            let base64_data = captures.name("data").unwrap().as_str();

            // Decode the base64 data
            let image_bytes = general_purpose::STANDARD.decode(base64_data)?;

            // Determine the file extension based on the MIME type
            let extension = match mime_type {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                _ => "bin", // Default to binary if unknown type
            };

            // Construct the filename using the timestamp and extension
            let filename =
                format!("tmp/fal_images/{}.{}", timestamp, extension);

            // Save the image bytes to a file asynchronously
            let mut file =
                File::create(&filename).await.with_context(|| {
                    format!("Error creating file: {}", filename)
                })?;
            file.write_all(&image_bytes).await.with_context(|| {
                format!("Error writing to file: {}", filename)
            })?;

            // **New Code Start**
            // Also save the image to "./tmp/dalle-1.png"
            let additional_filename = "./tmp/dalle-1.png";
            let mut additional_file =
                File::create(additional_filename).await.with_context(|| {
                    format!("Error creating file: {}", additional_filename)
                })?;
            additional_file.write_all(&image_bytes).await.with_context(
                || format!("Error writing to file: {}", additional_filename),
            )?;
            println!("Also saved to {}", additional_filename);
            // **New Code End**

            // Optionally save the image to an additional location
            if let Some(extra_folder) = extra_save_folder {
                let extra_filename =
                    format!("{}/{}.{}", extra_folder, timestamp, extension);
                let mut extra_file =
                    File::create(&extra_filename).await.with_context(|| {
                        format!("Error creating file: {}", extra_filename)
                    })?;
                extra_file.write_all(&image_bytes).await.with_context(
                    || format!("Error writing to file: {}", extra_filename),
                )?;
            }

            println!("Saved {}", filename);
        } else {
            eprintln!("Invalid data URL for image at index {}", index);
        }
    }

    Ok(())
}

pub async fn create_turbo_image_in_folder(
    prompt: String,
    suno_save_folder: &String,
) -> Result<()> {
    // Can I move this into it's own function that takes a prompt?
    // So here is as silly place I can run fal
    let client = FalClient::new(ClientCredentials::from_env());

    // let model = "fal-ai/stable-cascade";
    let model = "fal-ai/fast-turbo-diffusion";

    let res = client
        .run(
            model,
            serde_json::json!({
                "prompt": prompt,
                "image_size": "landscape_16_9",
            }),
        )
        .await
        .unwrap();

    let raw_json = res.bytes().await.unwrap();
    let timestamp = chrono::Utc::now().timestamp();
    let json_path = format!("tmp/fal_responses/{}.json", timestamp);
    tokio::fs::write(&json_path, &raw_json).await.unwrap();

    // This is not the folder
    // let save_folder = "tmp/fal_images";
    let _ = process_images(
        &timestamp.to_string(),
        &json_path,
        Some(&suno_save_folder),
    )
    .await;

    Ok(())
}

//// This is too specific
//pub async fn create_turbo_image(prompt: String) -> Result<()> {
//    // Can I move this into it's own function that takes a prompt?
//    // So here is as silly place I can run fal
//    let client = FalClient::new(ClientCredentials::from_env());
//
//    // let model = "fal-ai/stable-cascade/sote-diffusion";
//    // let model = "fal-ai/stable-cascade";
//    let model = "fal-ai/fast-turbo-diffusion";
//
//    let res = client
//        .run(
//            model,
//            serde_json::json!({
//                "prompt": prompt,
//                "image_size": "landscape_16_9",
//            }),
//        )
//        .await
//        .unwrap();
//
//    let raw_json = res.bytes().await.unwrap();
//    let timestamp = chrono::Utc::now().timestamp();
//    let json_path = format!("tmp/fal_responses/{}.json", timestamp);
//    tokio::fs::write(&json_path, &raw_json).await.unwrap();
//    let _ = process_images(&timestamp.to_string(), &json_path, None).await;
//
//    Ok(())
//}

#[cfg(test)]
mod tests {
    use super::*;
    //use crate::obs::obs;
    //use serde_json::{json, Error, Value};

    #[tokio::test]
    async fn test_parsing_fal() {
        // Saved w/ Text
        // let tmp_file_path = "tmp/fal_responses/1726345706.json";
        //
        // Saved with bytes
        let timestamp = "1726347150";
        let tmp_file_path = format!("tmp/fal_responses/{}.json", timestamp);

        process_images(&timestamp, &tmp_file_path, None)
            .await
            .unwrap();
    }

    //#[tokio::test]
    //async fn test_fal() {
    //    let prompt = "Magical Cat wearing a wizard hat";
    //    let _ = create_turbo_image(prompt.to_string()).await;
    //}
}
