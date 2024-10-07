pub mod models;
use anyhow::anyhow;
use anyhow::Result;
use serde::Serialize;
use sqlx::types::BigDecimal;
use sqlx::Error;

use sqlx::{postgres::PgPool, FromRow};
use uuid::Uuid;

#[derive(Serialize, Debug, FromRow)]
pub struct AiSongRanking {
    pub song_id: Uuid,
    pub title: String,
    pub avg_score: f64,
}

pub async fn total_votes_by_id(pool: &PgPool, song_id: Uuid) -> Result<i64> {
    let record = sqlx::query!(
        "
        SELECT COUNT(*) as count
        FROM ai_songs_vote
        WHERE song_id = $1
        ",
        song_id
    )
    .fetch_one(pool)
    .await?;
    record
        .count
        .ok_or(anyhow!("Error on ai_songs by song_id count"))
}

pub async fn find_random_song(
    pool: &PgPool,
) -> Result<ai_playlist::models::ai_songs::Model, Error> {
    sqlx::query_as!(
        ai_playlist::models::ai_songs::Model,
        r#"
        SELECT ai_songs.*
        FROM ai_songs
        LEFT JOIN ai_song_playlist ON ai_songs.song_id = ai_song_playlist.song_id
        WHERE ai_song_playlist.played_at IS NULL
           OR ai_song_playlist.played_at < NOW() - INTERVAL '1 hour'
        ORDER BY RANDOM()
        LIMIT 1
        "#
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::RowNotFound)
}

pub async fn get_random_high_rated_song(
    pool: &PgPool,
) -> Result<AiSongRanking> {
    let song = sqlx::query_as::<_, AiSongRanking>(
        r#"
        SELECT
            s.song_id,
            s.title,
            CAST(AVG(v.score) AS DOUBLE PRECISION) AS avg_score
        FROM
            ai_songs s
        JOIN
            ai_songs_vote v ON s.song_id = v.song_id
        GROUP BY
            s.song_id, s.title
        HAVING
            AVG(v.score) > 9.0
        ORDER BY
            RANDOM()
        LIMIT 1
        "#,
    )
    .fetch_one(pool)
    .await?;
    Ok(song)
}

// We might want a way to get dynamicd average score limits
pub async fn get_random_high_rated_recent_song(
    pool: &PgPool,
) -> Result<AiSongRanking, Error> {
    sqlx::query_as::<_, AiSongRanking>(
        r#"
        SELECT
            s.song_id,
            s.title,
            CAST(AVG(v.score) AS DOUBLE PRECISION) AS avg_score
        FROM
            ai_songs s
        JOIN
            ai_songs_vote v ON s.song_id = v.song_id
        LEFT JOIN
            ai_song_playlist p ON s.song_id = p.song_id
        WHERE
            p.played_at IS NULL OR p.played_at < NOW() - INTERVAL '1 hour'
        GROUP BY
            s.song_id, s.title
        HAVING
            AVG(v.score) > 9.0
        ORDER BY
            RANDOM()
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::RowNotFound)
}

pub async fn total_votes(pool: &PgPool) -> Result<i64> {
    let record = sqlx::query!(
        "
        SELECT COUNT(*) as count
        FROM ai_songs_vote
        ",
    )
    .fetch_one(pool)
    .await?;
    record.count.ok_or(anyhow!("Error on ai_songs count"))
}

pub async fn get_average_score(
    pool: &PgPool,
    song_id: Uuid,
) -> Result<AiSongRanking> {
    let ranking = sqlx::query_as::<_, AiSongRanking>(
        r#"
        SELECT
            s.song_id,
            s.title,
            CAST(AVG(v.score) AS DOUBLE PRECISION) AS avg_score
        FROM
            ai_songs s
        JOIN
            ai_songs_vote v ON s.song_id = v.song_id
        WHERE
            s.song_id = $1
        GROUP BY
            s.song_id, s.title
        ORDER BY
            avg_score DESC
        LIMIT 1
        "#,
    )
    .bind(song_id)
    .fetch_one(pool)
    .await?;
    Ok(ranking)
}

pub async fn get_top_songs(
    pool: &PgPool,
    limit: i64,
) -> Result<Vec<AiSongRanking>> {
    let songs = sqlx::query_as::<_, AiSongRanking>(
        r#"
        SELECT
            s.song_id,
            s.title,
            CAST(AVG(v.score) AS DOUBLE PRECISION) AS avg_score
        FROM
            ai_songs s
        JOIN
            ai_songs_vote v ON s.song_id = v.song_id
        GROUP BY
            s.song_id, s.title
        ORDER BY
            avg_score DESC
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    //
    //    SELECT
    //            s.song_id,
    //            s.title,
    //            CAST(AVG(v.score) AS DOUBLE PRECISION) AS avg_score,
    //            CAST(SUM(v.score) AS DOUBLE PRECISION) AS total_score,
    //            (COUNT(*) / 10) AS multipler
    //        FROM
    //            ai_songs s
    //        JOIN
    //            ai_songs_vote v ON s.song_id = v.song_id
    //        GROUP BY
    //            s.song_id, s.title
    //        ORDER BY
    //            avg_score DESC
    //;

    Ok(songs)
}

pub async fn vote_for_current_song_with_score(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    score: f64,
) -> Result<()> {
    let current_song = ai_playlist::get_current_song(pool).await?;
    let score = BigDecimal::try_from(score)?;

    let _ = models::find_or_create_and_save_score(
        pool,
        current_song.song_id,
        user_id,
        score,
    )
    .await;
    Ok(())
}
