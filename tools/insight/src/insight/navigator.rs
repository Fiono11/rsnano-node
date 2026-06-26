#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum NavItem {
    Peers,
    Messages,
    Queues,
    Representatives,
    BlockProcessor,
    Elections,
    Bootstrap,
    Explorer,
}

impl NavItem {
    pub fn name(&self) -> &'static str {
        match self {
            NavItem::Peers => "Peers",
            NavItem::Messages => "Messages",
            NavItem::Queues => "Queues",
            NavItem::Representatives => "Representatives",
            NavItem::BlockProcessor => "Block Processor",
            NavItem::Elections => "Elections",
            NavItem::Bootstrap => "Bootstrap",
            NavItem::Explorer => "Explorer",
        }
    }
}

pub(crate) static NAV_ORDER: [NavItem; 8] = [
    NavItem::Peers,
    NavItem::Messages,
    NavItem::Queues,
    NavItem::Representatives,
    NavItem::BlockProcessor,
    NavItem::Elections,
    NavItem::Bootstrap,
    NavItem::Explorer,
];

pub(crate) struct TabViewModel {
    pub selected: bool,
    pub label: &'static str,
    pub value: NavItem,
}
