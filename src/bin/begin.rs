use anyhow::Result;
use async_trait::async_trait;
use events::EventHandler;
use obws::Client as OBSClient;
use rodio::Decoder;
use rodio::*;
use serde::{Deserialize, Serialize};
use server::audio;
use server::move_transition;
use server::obs_combo;
use server::obs_hotkeys;
use server::obs_routing;
use server::obs_source;
use server::twitch_stream_state;
use server::uberduck;
use std::collections::HashSet;
use std::fs;
use std::fs::File;
use std::io::BufReader;
use std::thread;
use std::time;
use subd_db::get_db_pool;
use subd_types::Event;
use subd_types::TransformOBSTextRequest;
use subd_types::UberDuckRequest;
use tokio::sync::broadcast;
use tracing_subscriber;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

pub struct OBSMessageHandler {
    obs_client: OBSClient,
    pool: sqlx::PgPool,
}

pub struct TriggerHotkeyHandler {
    obs_client: OBSClient,
}

pub struct StreamCharacterHandler {
    obs_client: OBSClient,
}

pub struct SourceVisibilityHandler {
    obs_client: OBSClient,
}

pub struct TransformOBSTextHandler {
    obs_client: OBSClient,
}

pub struct SoundHandler {
    sink: Sink,
    pool: sqlx::PgPool,
}

// ================================================================================================

#[async_trait]
impl EventHandler for SourceVisibilityHandler {
    async fn handle(
        self: Box<Self>,
        _tx: broadcast::Sender<Event>,
        mut rx: broadcast::Receiver<Event>,
    ) -> Result<()> {
        loop {
            let event = rx.recv().await?;
            let msg = match event {
                Event::SourceVisibilityRequest(msg) => msg,
                _ => continue,
            };

            let _ = obs_source::set_enabled(
                &msg.scene,
                &msg.source,
                msg.enabled,
                &self.obs_client,
            )
            .await;
        }
    }
}

#[async_trait]
impl EventHandler for StreamCharacterHandler {
    async fn handle(
        self: Box<Self>,
        _tx: broadcast::Sender<Event>,
        mut rx: broadcast::Receiver<Event>,
    ) -> Result<()> {
        loop {
            let event = rx.recv().await?;
            let msg = match event {
                Event::StreamCharacterRequest(msg) => msg,
                _ => continue,
            };

            let _ = obs_combo::trigger_character_filters(
                &msg.source,
                &self.obs_client,
                msg.enabled,
            )
            .await;
        }
    }
}

#[async_trait]
impl EventHandler for TriggerHotkeyHandler {
    async fn handle(
        self: Box<Self>,
        _tx: broadcast::Sender<Event>,
        mut rx: broadcast::Receiver<Event>,
    ) -> Result<()> {
        loop {
            let event = rx.recv().await?;
            let msg = match event {
                Event::TriggerHotkeyRequest(msg) => msg,
                _ => continue,
            };

            obs_hotkeys::trigger_hotkey(&msg.hotkey, &self.obs_client).await?;
        }
    }
}

#[async_trait]
impl EventHandler for TransformOBSTextHandler {
    async fn handle(
        self: Box<Self>,
        _tx: broadcast::Sender<Event>,
        mut rx: broadcast::Receiver<Event>,
    ) -> Result<()> {
        loop {
            let event = rx.recv().await?;
            let msg = match event {
                Event::TransformOBSTextRequest(msg) => msg,
                _ => continue,
            };

            let filter_name = format!("Transform{}", msg.text_source);
            let _ = move_transition::update_and_trigger_text_move_filter(
                &msg.text_source,
                &filter_name,
                &msg.message,
                &self.obs_client,
            )
            .await;
        }
    }
}

// ================================================================================================

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Character {
    pub voice: Option<String>,
    pub source: Option<String>,
}

// Looks through raw-text to either play TTS or play soundeffects
#[async_trait]
impl EventHandler for SoundHandler {
    async fn handle(
        self: Box<Self>,
        tx: broadcast::Sender<Event>,
        mut rx: broadcast::Receiver<Event>,
    ) -> Result<()> {
        let paths = fs::read_dir("./MP3s").unwrap();
        let mut mp3s: HashSet<String> = vec![].into_iter().collect();
        for path in paths {
            mp3s.insert(path.unwrap().path().display().to_string());
        }

        loop {
            let event = rx.recv().await?;
            let msg = match event {
                Event::UserMessage(msg) => {
                    // TODO: Add a list here
                    if msg.user_name == "Nightbot" {
                        continue;
                    }
                    msg
                }
                _ => continue,
            };
            let spoken_string = msg.contents.clone();
            let voice_text = msg.contents.to_string();
            let speech_bubble_text = uberduck::chop_text(spoken_string);

            // Anything less than 3 words we don't use
            let split = voice_text.split(" ");
            let vec = split.collect::<Vec<&str>>();
            if vec.len() < 2 {
                continue;
            };

            let stream_character =
                uberduck::build_stream_character(&self.pool, &msg.user_name)
                    .await?;

            let state =
                twitch_stream_state::get_twitch_state(&self.pool).await?;

            let mut character = Character {
                ..Default::default()
            };

            // This is all about how to respond to messages from various
            // types of users
            if msg.roles.is_twitch_staff() {
                character.voice =
                    Some(server::obs::TWITCH_STAFF_OBS_SOURCE.to_string());
                character.source =
                    Some(server::obs::TWITCH_STAFF_VOICE.to_string());
            } else if msg.roles.is_twitch_mod() {
                character.voice =
                    Some(server::obs::TWITCH_MOD_DEFAULT_VOICE.to_string());
            } else if msg.roles.is_twitch_sub() {
                character.voice = Some(stream_character.voice.clone());
            } else if !state.sub_only_tts {
                // This is what everyone get's to speak with
                // if we are allowing non-subs to speak
                character.voice = Some(stream_character.voice.clone());
            }

            // If we have a voice assigned, then we fire off an UberDuck Request
            match character.voice {
                Some(voice) => {
                    let _ = tx.send(Event::UberDuckRequest(UberDuckRequest {
                        voice,
                        message: speech_bubble_text,
                        voice_text,
                        username: msg.user_name,
                        source: character.source,
                    }));
                }
                None => {}
            }

            // If we have the implicit_soundeffects enabled
            // we go past this!
            if !state.implicit_soundeffects {
                continue;
            }

            let splitmsg = msg
                .contents
                .split(" ")
                .map(|s| s.to_string())
                .collect::<Vec<String>>();

            let text_source =
                server::obs::SOUNDBOARD_TEXT_SOURCE_NAME.to_string();

            for word in splitmsg {
                let sanitized_word = word.as_str().to_lowercase();
                let full_name = format!("./MP3s/{}.mp3", sanitized_word);

                if mp3s.contains(&full_name) {
                    let _ = tx.send(Event::TransformOBSTextRequest(
                        TransformOBSTextRequest {
                            message: sanitized_word.clone(),
                            text_source: text_source.to_string(),
                        },
                    ));

                    let file = BufReader::new(
                        File::open(format!("./MP3s/{}.mp3", sanitized_word))
                            .unwrap(),
                    );

                    self.sink
                        .append(Decoder::new(BufReader::new(file)).unwrap());

                    self.sink.sleep_until_end();

                    // TODO: Look into using these!
                    // self.sink.volume()
                    // self.sink.set_volume()
                    // self.sink.len()

                    // We need this so we can allow to trigger the next word in OBS
                    // TODO: We should abstract
                    // and figure out a better way of determine the time
                    let sleep_time = time::Duration::from_millis(100);
                    thread::sleep(sleep_time);
                }
            }

            // This clears the OBS Text
            let _ = tx.send(Event::TransformOBSTextRequest(
                TransformOBSTextRequest {
                    message: "".to_string(),
                    text_source: text_source.to_string(),
                },
            ));
        }
    }
}

#[async_trait]
impl EventHandler for OBSMessageHandler {
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

            match obs_routing::handle_obs_commands(
                &tx,
                &self.obs_client,
                &self.pool,
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

// ==== //
// Main //
// ==== //

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        // .with_max_level(Level::TRACE)
        .with_env_filter(EnvFilter::new("chat=debug,server=debug"))
        .without_time()
        .with_target(false)
        .finish()
        .init();

    {
        use rustrict::{add_word, Type};

        // You must take care not to call these when the crate is being
        // used in any other way (to avoid concurrent mutation).
        unsafe {
            add_word(format!("vs{}", "code").as_str(), Type::PROFANE);
            add_word("vsc*de", Type::SAFE);
        }
    }

    // Advice!
    // codyphobe:
    //           For the OBSClient cloning,
    //           could you pass the OBSClient in the constructor when making event_loop,
    //           then pass self.obsclient into each handler's handle method inside
    //           EventLoop#run

    // Create 1 Event Loop
    // Push handles onto the loop
    // those handlers are things like twitch-chat, twitch-sub, github-sponsor etc.
    let mut event_loop = events::EventLoop::new();

    // You can clone this
    // because it's just adding one more connection per clone()???
    //
    // This is useful because you need no lifetimes
    let pool = subd_db::get_db_pool().await;

    // Turns twitch IRC things into our message events
    event_loop.push(twitch_chat::TwitchChat::new(
        pool.clone(),
        "beginbot".to_string(),
    )?);

    // Does stuff with twitch messages
    event_loop.push(twitch_chat::TwitchMessageHandler::new(
        pool.clone(),
        twitch_service::Service::new(
            pool.clone(),
            user_service::Service::new(pool.clone()).await,
        )
        .await,
    ));

    let obs_client = server::obs::create_obs_client().await?;
    event_loop.push(OBSMessageHandler {
        obs_client,
        pool: pool.clone(),
    });

    // Works for Arch Linux
    let (_stream, stream_handle) =
        audio::get_output_stream("pulse").expect("stream handle");
    // Works for Mac
    // let (_stream, handle) = rodio::OutputStream::try_default().unwrap();
    let sink = rodio::Sink::try_new(&stream_handle).unwrap();
    // This should be abstracted

    event_loop.push(SoundHandler {
        sink,
        pool: pool.clone(),
    });

    let sink = rodio::Sink::try_new(&stream_handle).unwrap();
    let pool = get_db_pool().await;
    event_loop.push(uberduck::UberDuckHandler { pool, sink });

    let obs_client = server::obs::create_obs_client().await?;
    event_loop.push(TriggerHotkeyHandler { obs_client });

    let obs_client = server::obs::create_obs_client().await?;
    event_loop.push(TransformOBSTextHandler { obs_client });

    let obs_client = server::obs::create_obs_client().await?;
    event_loop.push(StreamCharacterHandler { obs_client });

    let obs_client = server::obs::create_obs_client().await?;
    event_loop.push(SourceVisibilityHandler { obs_client });

    println!("\n\n\t\tLet's Start this Loop Up!");
    let _ = main2().await;
    event_loop.run().await?;

    Ok(())
}

use twitter_v2::api_result::ApiResponse;
use twitter_v2::authorization::{BearerToken, Oauth2Token};
use twitter_v2::id::NumericId;
use twitter_v2::query::{
    SpaceExpansion, SpaceField, TopicField, TweetField, UserField,
};
use twitter_v2::Space;
use twitter_v2::TwitterApi;

// |          --------- ^^^^^^^^^^ the trait `IntoNumericId` is not implemented for `&str`

async fn main2() -> Result<()> {
    let otherside_guild_id = NumericId::new(1521585633445122048);
    // let other_guild_tweet = 1607890390811840512;
    let phil_tweet = NumericId::new(1608588236452167681);

    let phil_id = NumericId::new(34440817);

    let auth =
        BearerToken::new(std::env::var("TWITTER_APP_BEARER_TOKEN").unwrap());

    // let tweet = TwitterApi::new(auth)
    //     .get_tweet(phil_tweet)
    //     .tweet_fields([TweetField::AuthorId, TweetField::CreatedAt])
    //     .send()
    //     .await?
    //     .into_data()
    //     .expect("this tweet should exist");
    // println!("{:?}", tweet);

    // let auth =
    //     BearerToken::new(std::env::var("TWITTER_APP_BEARER_TOKEN").unwrap());

    // So this ID doesn't seem right
    // We need to figure out a different one
    // println!("{:?}", space);
    //
    let auth =
        BearerToken::new(std::env::var("TWITTER_APP_BEARER_TOKEN").unwrap());

    // /(invited_user_ids, speaker_ids, creator_id, host_ids, topics_ids)/ expansions
    // 1ypKddPokoaKW
    // let space = TwitterApi::new(auth)
    //     .get_spaces_by_creator_ids([otherside_guild_id])
    //     // So these aren't working how I expected
    //     // I thought doing this could show the topics
    //     .topic_fields([
    //         TopicField::Id,
    //         TopicField::Name,
    //         TopicField::Description,
    //     ])
    //     .user_fields([
    //         UserField::CreatedAt,
    //         UserField::Description,
    //         UserField::Id,
    //         UserField::Name,
    //         // UserField::PinnedTweetId,
    //         // UserField::ProfileImageUrl,
    //         // UserField::PublicMetrics,
    //         UserField::Username,
    //         UserField::Verified,
    //         // Entities,
    //         // Location,
    //         // Protected,
    //         // Url,
    //         // Withheld,
    //     ])
    //     .expansions([
    //         SpaceExpansion::HostIds,
    //         // SpaceExpansion::InvitedUserIds,
    //         SpaceExpansion::SpeakerIds,
    //         SpaceExpansion::CreatorId,
    //     ])
    //     .space_fields([
    //         SpaceField::HostIds,
    //         SpaceField::CreatedAt,
    //         SpaceField::CreatorId,
    //         SpaceField::Id,
    //         // SpaceField::Lang,
    //         // SpaceField::InvitedUserIds,
    //         SpaceField::ParticipantCount,
    //         SpaceField::SpeakerIds,
    //         SpaceField::StartedAt,
    //         SpaceField::EndedAt,
    //         // SpaceField::SubscriberCount,
    //         SpaceField::TopicIds,
    //         SpaceField::State,
    //         SpaceField::Title,
    //         // SpaceField::UpdatedAt,
    //         SpaceField::ScheduledStart,
    //         // SpaceField::IsTicketed,
    //     ])
    //     .send()
    //     .await?
    //     .into_data()
    //     .expect("Space Not Found");
    // println!("\n\n\t\tSpace: {:?}", space);

    let auth =
        BearerToken::new(std::env::var("TWITTER_APP_BEARER_TOKEN").unwrap());

    let space = TwitterApi::new(auth)
        .get_space("1ypKddPokoaKW")
        .topic_fields([
            TopicField::Id,
            TopicField::Name,
            TopicField::Description,
        ])
        .user_fields([
            UserField::CreatedAt,
            UserField::Description,
            UserField::Id,
            UserField::Name,
            UserField::PinnedTweetId,
            UserField::ProfileImageUrl,
            UserField::PublicMetrics,
            UserField::Username,
            UserField::Verified,
            // Entities,
            // Location,
            // Protected,
            // Url,
            // Withheld,
        ])
        .expansions([
            SpaceExpansion::HostIds,
            SpaceExpansion::SpeakerIds,
            SpaceExpansion::CreatorId,
            // SpaceExpansion::InvitedUserIds,
        ])
        .space_fields([
            SpaceField::HostIds,
            SpaceField::CreatedAt,
            SpaceField::CreatorId,
            SpaceField::Id,
            SpaceField::ParticipantCount,
            SpaceField::SpeakerIds,
            SpaceField::StartedAt,
            SpaceField::EndedAt,
            SpaceField::TopicIds,
            SpaceField::State,
            SpaceField::Title,
            SpaceField::ScheduledStart,
            // SpaceField::Lang,
            // SpaceField::InvitedUserIds,
            // SpaceField::SubscriberCount,
            // SpaceField::UpdatedAt,
            // SpaceField::IsTicketed,
        ])
        .send()
        .await?;

    let includes = space.includes().expect("expected includes");
    println!("\n\n\t\tIncludes: {:?}", includes);

    // So How do I say plz lemme use this space
    let data = space.data().expect("expected data");
    println!("\n\n\t\tSpace: {:?}", data);

    // how do we use this space again

    // Why can't I use this again
    // let data = space.into_data().expect("Wha");
    // .into_data()
    // .into_meta()
    // .into_errors()

    // println!("\n\n\t\tSpace: {:?}", space);
    // println!("\n\n\t\tTopics: {:?}", space.into_includes);
    // println!("\n\n\t\tTopics: {:?}", space.topics);

    // println!("\t\tSpace: {:?}", space);

    // So I don't know how I should get a stored oauth2_token here
    // So how do we make a stored Oauthtoken here
    // let auth: Oauth2Token = serde_json::from_str(&stored_oauth2_token)?;
    Ok(())
}

// async fn find_includes(
//     space: &ApiResponse<BearerToken, Space, ()>,
// ) -> Result<()> {
//     // Can we return the space tho???
//     let includes = space.into_includes().expect("expected includes");
//     Ok(())
// }
