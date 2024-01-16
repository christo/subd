use crate::constants;
use crate::move_transition::duration;
use crate::move_transition::models;
use crate::move_transition::move_value;
use crate::obs_filters;
use anyhow::{Context, Result};
use obws::Client as OBSClient;

// This uses the source passed in to find a filter that starts w/ Move_
// and ends with the source
pub async fn update_and_trigger_filter<
    T: serde::Serialize + std::default::Default,
>(
    obs_client: &OBSClient,
    source: &str,
    filter_name: &str,
    settings: T,
    duration: duration::EasingDuration,
) -> Result<()> {
    let move_transition_filter_name = format!("Move_{}", filter_name);
    let settings =
        move_value::Settings::new(filter_name.to_string(), settings, duration);

    // ========================================
    update_filter_and_enable(
        source,
        &move_transition_filter_name,
        settings,
        obs_client,
    )
    .await
}

pub async fn spin_source(
    obs_client: &OBSClient,
    source: &str,
    rotation_z: f32,
    duration: duration::EasingDuration,
) -> Result<()> {
    let filter_name =
        constants::THREE_D_TRANSITION_PERSPECTIVE_FILTER_NAME.to_string();
    // This needs the builder pattern
    let three_d_settings =
        obs_filters::three_d_transform::ThreeDTransformPerspective {
            rotation_z: Some(rotation_z),
            ..Default::default()
        };
    let move_transition_filter_name = format!("Move_{}", source);

    // ========================================
    let settings = move_value::Settings::new(
        filter_name.clone(),
        three_d_settings,
        duration,
    );
    // ========================================
    update_filter_and_enable(
        source,
        &move_transition_filter_name,
        settings,
        obs_client,
    )
    .await
}

pub async fn move_source_in_scene_x_and_y(
    obs_client: &OBSClient,
    scene: &str,
    source: &str,
    x: f32,
    y: f32,
    duration: duration::EasingDuration,
) -> Result<()> {
    let filter_name = format!("Move_{}", source);

    // ========================================
    let settings = move_value::Settings::new(
        source.to_string(),
        models::Coordinates::new(Some(x), Some(y)),
        duration,
    );
    // ========================================
    update_filter_and_enable(scene, &filter_name, settings, obs_client).await
}

// ==================================================================================

pub async fn update_filter_and_enable<T: serde::Serialize>(
    source: &str,
    filter_name: &str,
    new_settings: T,
    obs_client: &obws::Client,
) -> Result<()> {
    update_filter(source, filter_name, new_settings, &obs_client)
        .await
        .context(format!(
            "Failed to update Filter: {} on Source: {}",
            filter_name, source
        ))?;
    let filter_enabled = obws::requests::filters::SetEnabled {
        source,
        filter: &filter_name,
        enabled: true,
    };
    Ok(obs_client.filters().set_enabled(filter_enabled).await?)
}

async fn update_filter<T: serde::Serialize>(
    source: &str,
    filter_name: &str,
    new_settings: T,
    obs_client: &OBSClient,
) -> Result<()> {
    let settings = obws::requests::filters::SetSettings {
        source,
        filter: filter_name,
        settings: Some(new_settings),
        overlay: Some(true),
    };
    Ok(obs_client.filters().set_settings(settings).await?)
}
