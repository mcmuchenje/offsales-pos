-- Which products each till sells. A product with no rows here is sold at
-- every till; otherwise only at the tills listed.
CREATE TABLE product_tills (
    product_id INTEGER NOT NULL REFERENCES products(id) ON DELETE CASCADE,
    device_id  INTEGER NOT NULL,
    PRIMARY KEY (product_id, device_id)
);

-- On a till: the server's list of tills, so a manager can assign products
-- from any till.
CREATE TABLE known_tills (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL
);
