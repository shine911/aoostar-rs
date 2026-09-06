use crate::{IconSource, TIError};
use ksni::{menu::StandardItem, Handle, Icon};
use std::sync::Arc;

enum TrayItem {
    Label { id: u32, label: String },
    MenuItem {
        id: u32,
        label: String,
        action: Arc<dyn Fn() + Send + Sync + 'static>,
    },
    Separator,
}

struct Tray {
    title: String,
    icon: IconSource,
    actions: Vec<TrayItem>,
}

pub struct TrayItemLinux {
    tray: Handle<Tray>,
    // ksni::Handle::update queues the mutation asynchronously. Keep the
    // sequence in this caller instead of reading it back from the queued
    // closure, which previously returned 0 for every item under normal KDE
    // scheduling and made later label/checkmark updates target nothing.
    next_id: u32,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        self.title.clone()
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn icon_name(&self) -> String {
        match &self.icon {
            IconSource::Resource(name) => name.to_string(),
            IconSource::Data { .. } => String::new(),
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        match &self.icon {
            IconSource::Resource(_) => vec![],
            IconSource::Data {
                data,
                height,
                width,
            } => {
                vec![Icon {
                    width: *height,
                    height: *width,
                    data: data.clone(),
                }]
            }
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        self.actions
            .iter()
            .map(|item| match item {
                TrayItem::Label { label, .. } => StandardItem {
                    label: label.clone(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                TrayItem::MenuItem { label, action, .. } => {
                    let action = action.clone();
                    StandardItem {
                        label: label.clone(),
                        activate: Box::new(move |_| {
                            action();
                        }),
                        ..Default::default()
                    }
                    .into()
                }
                TrayItem::Separator => ksni::MenuItem::Separator,
            })
            .collect()
    }
}

impl TrayItemLinux {
    pub fn new(title: &str, icon: IconSource) -> Result<Self, TIError> {
        let svc = ksni::TrayService::new(Tray {
            title: title.to_string(),
            icon,
            actions: vec![],
        });

        let handle = svc.handle();
        svc.spawn();

        Ok(Self {
            tray: handle,
            next_id: 0,
        })
    }

    pub fn set_icon(&mut self, icon: IconSource) -> Result<(), TIError> {
        self.tray.update(|tray| tray.icon = icon.clone());

        Ok(())
    }

    pub fn add_label(&mut self, label: &str) -> Result<(), TIError> {
        self.add_label_with_id(label)?;
        Ok(())
    }

    pub fn add_label_with_id(&mut self, label: &str) -> Result<u32, TIError> {
        let item_id = self.allocate_id();
        self.tray.update(move |tray| {
            tray.actions.push(TrayItem::Label {
                id: item_id,
                label: label.to_string(),
            });
        });
        Ok(item_id)
    }

    pub fn add_menu_item<F>(&mut self, label: &str, cb: F) -> Result<(), TIError>
    where
        F: Fn() -> () + Send + Sync + 'static,
    {
        self.add_menu_item_with_id(label, cb)?;
        Ok(())
    }

    pub fn add_menu_item_with_id<F>(&mut self, label: &str, cb: F) -> Result<u32, TIError>
    where
        F: Fn() + Send + Sync + 'static,
    {
        let action = Arc::new(cb);
        let item_id = self.allocate_id();

        self.tray.update(move |tray| {
            tray.actions.push(TrayItem::MenuItem {
                id: item_id,
                label: label.to_string(),
                action: action.clone(),
            });
        });

        Ok(item_id)
    }

    fn allocate_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    pub fn set_menu_item_label(&mut self, label: &str, id: u32) -> Result<(), TIError> {
        self.tray.update(move |tray| {
            for item in &mut tray.actions {
                if let TrayItem::MenuItem { id: item_id, label: item_label, .. } = item {
                    if *item_id == id { *item_label = label.to_string(); break; }
                }
                if let TrayItem::Label { id: item_id, label: item_label } = item {
                    if *item_id == id { *item_label = label.to_string(); break; }
                }
            }
        });

        Ok(())
    }

    pub fn add_separator(&mut self) -> Result<(), TIError> {
        self.tray.update(move |tray| {
            tray.actions.push(TrayItem::Separator);
        });

        Ok(())
    }
}
