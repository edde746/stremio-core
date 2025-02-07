use std::collections::{hash_map::Entry, HashMap};

use either::Either;
use futures::FutureExt;
use lazysort::SortedBy;

use crate::{
    constants::{LAST_VIDEOS_IDS_EXTRA_PROP, NOTIFICATIONS_STORAGE_KEY, NOTIFICATION_ITEMS_COUNT},
    models::{
        common::{
            eq_update, resources_update_with_vector_content, Loadable, ResourceLoadable,
            ResourcesAction,
        },
        ctx::{CtxError, CtxStatus},
    },
    runtime::{
        msg::{Action, ActionCtx, Event, Internal, Msg},
        Effect, EffectFuture, Effects, Env, EnvFutureExt,
    },
    types::{
        addon::{AggrRequest, ExtraValue},
        library::LibraryBucket,
        notifications::{NotificationItem, NotificationsBucket},
        profile::Profile,
        resource::{MetaItem, MetaItemId, VideoId},
    },
};

pub fn update_notifications<E: Env + 'static>(
    notifications: &mut NotificationsBucket,
    notification_catalogs: &mut Vec<ResourceLoadable<Vec<MetaItem>>>,
    profile: &Profile,
    library: &LibraryBucket,
    status: &CtxStatus,
    msg: &Msg,
) -> Effects {
    match msg {
        Msg::Action(Action::Ctx(ActionCtx::PullNotifications)) => {
            let library_item_ids = library
                .items
                .values()
                .filter(|library_item| library_item.should_pull_notifications())
                .sorted_by(|a, b| b.mtime.cmp(&a.mtime))
                .take(NOTIFICATION_ITEMS_COUNT)
                .map(|library_item| &library_item.id)
                .cloned()
                .collect::<Vec<_>>();

            let notifications_catalog_resource_effects = if !library_item_ids.is_empty() {
                resources_update_with_vector_content::<E, _>(
                    notification_catalogs,
                    // force the making of a requests every time PullNotifications is called.
                    ResourcesAction::force_request(
                        &AggrRequest::AllCatalogs {
                            extra: &vec![ExtraValue {
                                name: LAST_VIDEOS_IDS_EXTRA_PROP.name.to_owned(),
                                value: library_item_ids.join(","),
                            }],
                            r#type: &None,
                        },
                        &profile.addons,
                    ),
                )
            } else {
                Effects::none().unchanged()
            };

            notifications.last_updated = Some(E::now());

            // first update the notification items
            let notification_items_effects = update_notification_items::<E>(
                &mut notifications.items,
                notification_catalogs,
                library,
            );

            // because notifications are getting loaded by forcing new requests
            // we do not trigger a `NotificationsChanged` as the addons should return results first.
            notifications_catalog_resource_effects
                .join(notification_items_effects)
                .unchanged()
        }
        Msg::Action(Action::Ctx(ActionCtx::DismissNotificationItem(id))) => Effects::msg(
            Msg::Internal(Internal::DismissNotificationItem(id.to_owned())),
        )
        .unchanged(),
        Msg::Action(Action::Ctx(ActionCtx::Logout)) | Msg::Internal(Internal::Logout) => {
            let notification_catalogs_effects = eq_update(notification_catalogs, vec![]);
            let next_notifications = NotificationsBucket::new::<E>(profile.uid(), vec![]);
            let notifications_effects = if *notifications != next_notifications {
                *notifications = next_notifications;
                Effects::msg(Msg::Internal(Internal::NotificationsChanged))
            } else {
                Effects::none().unchanged()
            };
            notification_catalogs_effects
                .join(notifications_effects)
                .unchanged()
        }
        Msg::Internal(Internal::CtxAuthResult(auth_request, result)) => match (status, result) {
            (CtxStatus::Loading(loading_auth_request), Ok(_))
                if loading_auth_request == auth_request =>
            {
                let notification_catalogs_effects = eq_update(notification_catalogs, vec![]);
                let next_notifications = NotificationsBucket::new::<E>(profile.uid(), vec![]);
                let notifications_effects = if *notifications != next_notifications {
                    *notifications = next_notifications;
                    Effects::msg(Msg::Internal(Internal::NotificationsChanged))
                } else {
                    Effects::none().unchanged()
                };
                notification_catalogs_effects
                    .join(notifications_effects)
                    .unchanged()
            }
            _ => Effects::none().unchanged(),
        },
        Msg::Internal(Internal::ResourceRequestResult(request, result)) => {
            let notification_catalogs_effects = resources_update_with_vector_content::<E, _>(
                notification_catalogs,
                ResourcesAction::ResourceRequestResult { request, result },
            );
            let notification_items_effects = if notification_catalogs_effects.has_changed {
                update_notification_items::<E>(
                    &mut notifications.items,
                    notification_catalogs,
                    library,
                )
            } else {
                Effects::none().unchanged()
            };

            let notifications_effects = if notification_items_effects.has_changed {
                Effects::msg(Msg::Internal(Internal::NotificationsChanged))
            } else {
                Effects::none().unchanged()
            };
            notification_catalogs_effects
                .join(notification_items_effects)
                .join(notifications_effects)
        }
        Msg::Internal(Internal::DismissNotificationItem(id)) => {
            dismiss_notification_item::<E>(library, notifications, id)
        }
        Msg::Internal(Internal::NotificationsChanged) => {
            Effects::one(push_notifications_to_storage::<E>(notifications)).unchanged()
        }
        _ => Effects::none().unchanged(),
    }
}

fn update_notification_items<E: Env + 'static>(
    notification_items: &mut HashMap<MetaItemId, HashMap<VideoId, NotificationItem>>,
    notification_catalogs: &[ResourceLoadable<Vec<MetaItem>>],
    library: &LibraryBucket,
) -> Effects {
    let selected_catalogs = notification_catalogs
        .iter()
        // take any catalog while the catalog has successful result or resulted in error
        .take_while(|catalog| {
            matches!(
                &catalog.content,
                Some(Loadable::Ready(_)) | Some(Loadable::Err(_))
            )
        })
        .collect::<Vec<_>>();

    // Get next notifications ids from lastVideosIds request's extra value
    let next_notification_ids = notification_catalogs
        .first()
        .map(|resource| &resource.request.path.extra)
        .map(|extra| Either::Left(extra.iter()))
        .unwrap_or_else(|| Either::Right(std::iter::empty()))
        .find(|extra_value| extra_value.name == LAST_VIDEOS_IDS_EXTRA_PROP.name)
        .map(|extra_value| Either::Left(extra_value.value.split(',')))
        .unwrap_or_else(|| Either::Right(std::iter::empty()));

    let next_notification_items = next_notification_ids.fold(HashMap::new(), |mut map, meta_id| {
        // Get the LibraryItem from user's library
        // Exit early if library item does not exist in the Library
        // or we do not need to pull notifications for it
        let library_item = match library.items.get(meta_id) {
            Some(library_item) if library_item.should_pull_notifications() => library_item,
            _ => return map,
        };

        // find the first occurrence of the meta item inside the catalogs
        let meta_item = match selected_catalogs.iter().find_map(|catalog| {
            catalog
                .content
                .as_ref()
                .and_then(|content| content.ready())
                .and_then(|content| {
                    content
                        .iter()
                        .find(|meta_item| meta_item.preview.id == meta_id)
                })
        }) {
            Some(meta_item) if !meta_item.videos.is_empty() => meta_item,
            _ => return map,
        };

        let mut meta_notifs: &mut HashMap<_, _> = map.entry(meta_id.to_string()).or_default();

        // meta items videos
        meta_item
            .videos_iter()
            .filter_map(|video| {
                match (&library_item.state.last_watched, video.released) {
                    (Some(last_watched), Some(video_released)) => {
                        if last_watched < &video_released &&
                                    // exclude future videos (i.e. that will air in the future)
                                    video_released <= E::now()
                        {
                            Some((&library_item.id, &video.id, video_released))
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            })
            // We need to manually fold, otherwise the last seen element with a given key
            // will be present in the final HashMap instead of the first occurrence.
            .fold(
                &mut meta_notifs,
                |meta_notifs, (meta_id, video_id, video_released)| {
                    let notif_entry = meta_notifs.entry(video_id.to_owned());

                    // for now just skip same videos that already exist
                    // leave the first one found in the Vec.
                    if let Entry::Vacant(new) = notif_entry {
                        let notification = NotificationItem {
                            meta_id: meta_id.to_owned(),
                            video_id: video_id.to_owned(),
                            video_released,
                        };

                        new.insert(notification);
                    }

                    meta_notifs
                },
            );

        // if not videos were added and the hashmap is empty, just remove the MetaItem record all together
        if meta_notifs.is_empty() {
            map.remove(meta_id);
        }

        map
    });

    eq_update(notification_items, next_notification_items)
}

fn push_notifications_to_storage<E: Env + 'static>(notifications: &NotificationsBucket) -> Effect {
    let ids = notifications.items.keys().cloned().collect();
    EffectFuture::Sequential(
        E::set_storage(NOTIFICATIONS_STORAGE_KEY, Some(notifications))
            .map(move |result| match result {
                Ok(_) => Msg::Event(Event::NotificationsPushedToStorage { ids }),
                Err(error) => Msg::Event(Event::Error {
                    error: CtxError::from(error),
                    source: Box::new(Event::NotificationsPushedToStorage { ids }),
                }),
            })
            .boxed_env(),
    )
    .into()
}

fn dismiss_notification_item<E: Env + 'static>(
    library: &LibraryBucket,
    notifications: &mut NotificationsBucket,
    id: &str,
) -> Effects {
    match notifications.items.remove(id) {
        Some(_) => {
            // when dismissing notifications, make sure we update the `last_watched`
            // of the LibraryItem this way if we've only `DismissedNotificationItem`
            // the next time we `PullNotifications` we won't see the same notifications.
            let library_item_effects = match library.items.get(id) {
                Some(library_item) => {
                    let mut library_item = library_item.to_owned();
                    library_item.state.last_watched = Some(E::now());

                    Effects::msg(Msg::Internal(Internal::UpdateLibraryItem(library_item)))
                        .unchanged()
                }
                _ => Effects::none().unchanged(),
            };

            Effects::msg(Msg::Internal(Internal::NotificationsChanged))
                .join(library_item_effects)
                .join(Effects::msg(Msg::Event(Event::NotificationsDismissed {
                    id: id.to_owned(),
                })))
        }
        _ => Effects::none().unchanged(),
    }
}
