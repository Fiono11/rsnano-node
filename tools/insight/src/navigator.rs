#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum NavItem {
    Peers,
    Messages,
    Queues,
    BlockProcessor,
    Elections,
    Bootstrap,
    FrontierScan,
    Explorer,
}

impl NavItem {
    pub fn name(&self) -> &'static str {
        match self {
            NavItem::Peers => "Peers",
            NavItem::Messages => "Messages",
            NavItem::Queues => "Queues",
            NavItem::BlockProcessor => "Block Processor",
            NavItem::Elections => "Elections",
            NavItem::Bootstrap => "Bootstrap",
            NavItem::FrontierScan => "Frontier Scan",
            NavItem::Explorer => "Explorer",
        }
    }
}

static NAV_ORDER: [NavItem; 7] = [
    NavItem::Peers,
    NavItem::Messages,
    NavItem::Queues,
    //NavItem::BlockProcessor,
    NavItem::Elections,
    NavItem::Bootstrap,
    NavItem::FrontierScan,
    NavItem::Explorer,
];

pub(crate) struct Navigator {
    pub current: NavItem,
    pub all: Vec<NavItem>,
}

impl Navigator {
    pub(crate) fn new() -> Self {
        Self {
            current: NavItem::Peers,
            all: NAV_ORDER.into(),
        }
    }
}
