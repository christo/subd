use anyhow::{anyhow, Result};
use reqwest::Client;
use rodio::{Decoder, Sink};
use sqlx::types::Uuid;
use std::fs::File;
use std::io::BufReader;
use tokio::fs;
use tokio::sync::broadcast;
use twitch_chat::client::send_message;
use twitch_irc::{
    login::StaticLoginCredentials, SecureTCPTransport, TwitchIRCClient,
};

pub mod models;

#[derive(Default, Debug, serde::Serialize)]
pub struct AudioGenerationData {
    pub prompt: String,
    pub make_instrumental: bool,
    pub wait_audio: bool,
}

/// Plays audio based on the provided song ID.
pub async fn play_audio(
    twitch_client: &TwitchIRCClient<SecureTCPTransport, StaticLoginCredentials>,
    pool: &sqlx::PgPool,
    sink: &Sink,
    id: &str,
    user_name: &str,
) -> Result<()> {
    println!("\tQueuing {}", id);
    let info = format!("@{} added {} to Queue", user_name, id);
    send_message(twitch_client, info).await?;

    let file_name = format!("ai_songs/{}.mp3", id);
    let mp3 = File::open(&file_name).map_err(|e| {
        anyhow!("Error opening sound file {}: {}", file_name, e)
    })?;
    let file = BufReader::new(mp3);
    println!("\tPlaying Audio {}", id);

    let uuid_id = Uuid::parse_str(id)
        .map_err(|e| anyhow!("Invalid UUID {}: {}", id, e))?;

    println!("Adding to Playlist");
    ai_playlist::add_song_to_playlist(pool, uuid_id).await?;
    ai_playlist::mark_song_as_played(pool, uuid_id).await?;

    play_sound_instantly(sink, file).await?;

    Ok(())
}

/// Retrieves audio information based on the song ID.
pub async fn get_audio_information(id: &str) -> Result<models::SunoResponse> {
    let base_url = "http://localhost:3000";
    let url = format!("{}/api/get?ids={}", base_url, id);

    let client = Client::new();
    let response = client.get(&url).send().await?;
    let suno_response: Vec<models::SunoResponse> = response.json().await?;

    suno_response
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("No audio information found"))
}

/// Plays sound instantly by appending it to the sink.
pub async fn play_sound_instantly(
    sink: &Sink,
    file: BufReader<File>,
) -> Result<()> {
    match Decoder::new(file) {
        Ok(decoder) => {
            println!("\tAppending Sound");
            sink.append(decoder);
            Ok(())
        }
        Err(e) => Err(anyhow!("Error decoding sound file: {}", e)),
    }
}

/// Generates audio based on the provided prompt.
pub async fn generate_audio_by_prompt(
    data: AudioGenerationData,
) -> Result<serde_json::Value> {
    let base_url = "http://localhost:3000/api/generate";
    let client = Client::new();

    let response = client
        .post(base_url)
        .json(&data)
        .header("Content-Type", "application/json")
        .send()
        .await?;
    let raw_json = response.text().await?;
    let tmp_file_path =
        format!("tmp/suno_responses/{}.json", chrono::Utc::now().timestamp());
    fs::write(&tmp_file_path, &raw_json).await?;
    println!("Raw JSON saved to: {}", tmp_file_path);

    serde_json::from_str::<serde_json::Value>(&raw_json).map_err(Into::into)
}

/// Downloads the song and initiates playback.
pub async fn download_and_play(
    twitch_client: &TwitchIRCClient<SecureTCPTransport, StaticLoginCredentials>,
    tx: &broadcast::Sender<subd_types::Event>,
    user_name: String,
    id: &String,
) -> Result<()> {
    let id = id.clone();
    let tx = tx.clone();
    let twitch_client = twitch_client.clone();

    tokio::spawn(async move {
        let cdn_url = format!("https://cdn1.suno.ai/{}.mp3", id);
        loop {
            println!(
                "{} | Attempting to Download song at: {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                cdn_url
            );
            match reqwest::get(&cdn_url).await {
                Ok(response) if response.status().is_success() => {
                    if let Err(e) = just_download(response, id.clone()).await {
                        eprintln!("Error downloading file: {}", e);
                    }

                    let info = format!(
                        "@{}'s song {} added to the Queue.",
                        user_name, id
                    );

                    if let Err(e) = send_message(&twitch_client, info).await {
                        eprintln!("Error sending message: {}", e);
                    }

                    let _ = tx.send(subd_types::Event::UserMessage(
                        subd_types::UserMessage {
                            user_name: "beginbot".to_string(),
                            contents: format!("!play {}", id),
                            ..Default::default()
                        },
                    ));

                    break;
                }
                Ok(_) => {
                    println!("Song not ready yet, retrying in 5 seconds...");
                }
                Err(e) => {
                    eprintln!("Error fetching song: {}", e);
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
    Ok(())
}

/// Parses the Suno response, saves song information, and initiates download and playback.
pub async fn parse_suno_response_download_and_play(
    twitch_client: &TwitchIRCClient<SecureTCPTransport, StaticLoginCredentials>,
    pool: &sqlx::PgPool,
    tx: &broadcast::Sender<subd_types::Event>,
    json_response: serde_json::Value,
    index: usize,
    user_name: String,
) -> Result<()> {
    let song_data = json_response
        .get(index)
        .ok_or_else(|| anyhow!("No song data at index {}", index))?;

    let id = song_data
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing 'id' in song data"))?;

    let song_id = Uuid::parse_str(id)
        .map_err(|e| anyhow!("Invalid UUID {}: {}", id, e))?;

    let lyrics = song_data
        .get("lyric")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let title = song_data
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt = song_data
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tags = song_data.get("tags").and_then(|v| v.as_str()).unwrap_or("");
    let audio_url = song_data
        .get("audio_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let gpt_description_prompt = song_data
        .get("gpt_description_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let created_at = sqlx::types::time::OffsetDateTime::now_utc();
    let new_song = ai_playlist::models::ai_songs::Model {
        song_id,
        title: title.to_string(),
        tags: tags.to_string(),
        prompt: prompt.to_string(),
        username: user_name.clone(),
        audio_url: audio_url.to_string(),
        lyric: Some(lyrics.to_string()),
        gpt_description_prompt: gpt_description_prompt.to_string(),
        last_updated: Some(created_at),
        created_at: Some(created_at),
    };
    new_song.save(pool).await?;

    let folder_path = format!("tmp/suno_responses/{}", id);
    fs::create_dir_all(&folder_path).await?;

    fs::write(
        format!("tmp/suno_responses/{}.json", id),
        &json_response.to_string(),
    )
    .await?;

    download_and_play(twitch_client, tx, user_name, &id.to_string()).await
}

/// Downloads the audio file and saves it locally.
pub async fn just_download(
    response: reqwest::Response,
    id: String,
) -> Result<BufReader<File>> {
    let file_name = format!("ai_songs/{}.mp3", id);
    let mut file = fs::File::create(&file_name).await?;

    let content = response.bytes().await?;
    tokio::io::copy(&mut &content[..], &mut file).await?;
    println!("Downloaded audio to: {}", file_name);

    let mp3 = File::open(&file_name).map_err(|e| {
        anyhow!("Error opening sound file {}: {}", file_name, e)
    })?;
    let file = BufReader::new(mp3);

    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    #[ignore]
    async fn test_parsing_json() {
        let f = fs::read_to_string("tmp/raw_response.json")
            .expect("Failed to open file");
        let suno_responses: Vec<models::SunoResponse> =
            serde_json::from_str(&f).expect("Failed to parse JSON");

        assert!(!suno_responses.is_empty());
        assert_eq!(suno_responses[0].status, "completed");
    }
}
