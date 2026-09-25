-- declines: not a MIN or a MAX
-- One aggregate that is not an extreme is enough to keep the whole join.
SELECT MIN(t.title) AS movie_title, COUNT(*) AS n FROM title AS t, movie_keyword AS mk WHERE t.id = mk.movie_id;
