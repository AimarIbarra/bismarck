use poise::serenity_prelude as serenity;
use serenity::{ActivityData, CreateAllowedMentions};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::info;

use crate::utilities::types::GuildSettings;
use crate::{Data, Error};

pub async fn event_handler(
    context: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    match event {
        serenity::FullEvent::Ready { data_about_bot, .. } => {
            let ready = data_about_bot;
            let http = &context.http;

            let api_version = ready.version;
            let bot_gateway = http.get_bot_gateway().await.unwrap();
            let bot_owner = http
                .get_current_application_info()
                .await
                .unwrap()
                .owner
                .expect("Couldn't get bot owner");
            let t_sessions = bot_gateway.session_start_limit.total;
            let r_sessions = bot_gateway.session_start_limit.remaining;
            let shard_info = ready.shard.unwrap();

            info!("Successfully logged into Discord as the following user:");
            info!("Bot username: {}", ready.user.tag());
            info!("Bot user ID: {}", ready.user.id);
            info!("Bot owner: {}", bot_owner.tag());

            let guild_count = ready.guilds.len();

            info!(
                "Connected to shard {} out of a total of {} shards.",
                shard_info.id, shard_info.total
            );
            info!("Connected to the Discord API (version {api_version}) with {r_sessions}/{t_sessions} sessions remaining.");
            info!("Connected to and serving a total of {guild_count} guild(s).");
        }
        serenity::FullEvent::Message { new_message } => {
            if new_message.author.bot {
                return Ok(());
            }

            // trim the end to make it easier for mobile users
            let content = new_message.content.trim_end();

            if content == "<@!1183487567094632638>" || content == "<@1183487567094632638>" {
                let prefix = {
                    let guild_data = &data.guild_data;
                    let pf = guild_data;
                    pf.get(&new_message.guild_id.unwrap().get())
                        .unwrap()
                        .prefix
                        .clone()
                };

                let embed = serenity::builder::CreateEmbed::new()
                    .title("**Hello!**")
                    .description(format!(
                        "```To see the list of commands type {}help```",
                        prefix
                    ));

                let builder = serenity::builder::CreateMessage::new()
                    .add_embed(embed)
                    .allowed_mentions(
                        CreateAllowedMentions::new().users(vec![new_message.author.id]),
                    )
                    .reference_message(new_message);

                new_message
                    .channel_id
                    .send_message(&context, builder)
                    .await
                    .unwrap();
            }
        }
        serenity::FullEvent::ThreadCreate { thread } => {
            if let Err(err) = thread.id.join_thread(&context.http).await {
                let thread_id = thread.id;
                info!("Failed to succesfully join thread (ID: {thread_id}): {err}")
            } else {
                let name = &thread.name;
                let guild = &thread.guild(&context.cache).unwrap().name;
                let id = thread.id.get();
                info!("Joined new thread: {name} (Server: {guild}, ID: {id})")
            }
        }
        serenity::FullEvent::CacheReady { guilds } => {
            info!("Cache is ready with {} guilds", guilds.len());

            // We need to check that the loop is not already running when this event triggers, as this
            // event triggers every time the bot enters or leaves a guild, along every time the ready
            // shard event triggers.
            if !data.is_loop_running.load(Ordering::Relaxed) {
                // And of course, we can run more than one thread at different timings.'
                let guild_len = guilds.len();
                let cloned = context.clone();

                tokio::spawn(async move {
                    loop {
                        set_activity(&cloned, guild_len);
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        set_ad(&cloned);
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                });

                // Now that the loop is running, we set the bool to true
                data.is_loop_running.swap(true, Ordering::Relaxed);
            }
        }
        serenity::FullEvent::GuildCreate { guild, is_new: _ } => {
            // write into database and hashmap
            info!("Connected to guild: {}", guild.name);
            info!("Guild ID: {}", guild.id);
            info!("Guild Owner ID: {}", guild.owner_id);
            info!("Guild Members: {}", guild.member_count);

            let database = data.sqlite.clone();
            let (guild_id, owner_id) = {
                let guild_id = i64::from(guild.id);
                let owner_id = i64::from(guild.owner_id);

                (guild_id, owner_id)
            };

            let query = sqlx::query!(
                "INSERT INTO guild_settings (
                    guild_id,
                    prefix,
                    owner_id
                ) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
                guild_id,
                "+",
                owner_id
            )
            .execute(&database)
            .await
            .unwrap();

            let bot_stat_query = sqlx::query!(
                "INSERT INTO bot_stats (
                    guild_id,
                    commands_ran,
                    songs_played
                ) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
                guild_id,
                0,
                0
            )
            .execute(&database)
            .await
            .unwrap();

            info!("Guild Settings Query: {query:?}");
            info!("Bot Stats Query: {bot_stat_query:?}");

            let fetched_guild =
                sqlx::query!("SELECT * FROM guild_settings WHERE guild_id = ?", guild_id,)
                    .fetch_one(&database)
                    .await
                    .unwrap();

            let fetched_bot_stats =
                sqlx::query!("SELECT * FROM bot_stats WHERE guild_id = ?", guild_id,)
                    .fetch_one(&database)
                    .await
                    .unwrap();

            let owner_id_u64 = owner_id as u64;
            let guild_id_u64 = guild_id as u64;

            let commands_ran = fetched_bot_stats.commands_ran as u64;
            let songs_played = fetched_bot_stats.songs_played as u64;

            data.commands_ran
                .insert(guild_id_u64, AtomicU64::new(commands_ran));
            data.songs_played
                .insert(guild_id_u64, AtomicU64::new(songs_played));

            let data_to_set = GuildSettings {
                prefix: fetched_guild.prefix,
                owner_id: owner_id_u64,
                mute_type: fetched_guild.mute_style.to_string(),
                mute_role: fetched_guild.mute_role_id.unwrap_or_default() as u64,
                default_mute_duration: fetched_guild.mute_duration as u64,
            };

            {
                let guild_settings = &data.guild_data;
                guild_settings.insert(guild_id_u64, data_to_set);
            }

            info!("Guild settings set complete for guild {}", guild.name);
        }
        serenity::FullEvent::GuildDelete {
            incomplete: _,
            full,
        } => {
            let guild = full.clone().unwrap();
            info!("Left guild: {}", guild.name);
            // write into database and hashmap
            {
                let database = data.sqlite.clone();
                let guild_id = i64::from(guild.id);
                sqlx::query!("DELETE FROM guild_settings WHERE guild_id = ?", guild_id)
                    .execute(&database)
                    .await
                    .unwrap();
            }
        }
        _ => {}
    }
    Ok(())
}

fn set_activity(context: &serenity::Context, guild_count: usize) {
    let presence = format!("Monitoring a total of {guild_count} guilds | -help");

    context.set_activity(Some(ActivityData::playing(presence)));
}

fn set_ad(context: &serenity::Context) {
    let presence = format!("On Shard {} | Flottenstützpunkt Hamburg", context.shard_id);

    context.set_activity(Some(ActivityData::listening(presence)));
}
